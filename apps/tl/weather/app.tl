app so.tilo.weather "Weather" (
    icon: .cloud_sun,
    category: .utility,
    tagline: "Forecasts for the places you follow",
    keywords: ["weather", "forecast", "temperature", "rain", "wind", "snow", "wetter", "vorhersage", "temperatur", "regen", "schnee"],
    splash_budget: 250,
    web: ["api.open-meteo.com", "geocoding-api.open-meteo.com", "air-quality-api.open-meteo.com"],
)

use so.tilo.settings.sections as settings

/// One hour of the hours ahead, as the place's own clock reads it.
type Hour {
    /// The place's local time, `2026-06-30T14:00`.
    at: Text = ""
    temperature: Float = 0.0
    /// How likely rain is, in percent.
    chance: Float = 0.0
    /// The weather code the hour carries.
    code: Int = 0
    /// Whether it is daylight then.
    day: Bool = true
    /// The glyph the hour shows, by the icon set's name.
    icon: Text = "cloud"
}

/// One day of the week ahead.
type Day {
    /// The day itself, `2026-06-30`.
    date: Text = ""
    /// Its weekday, counted from Monday as 0: what names it once it is neither today nor
    /// tomorrow.
    weekday: Int = 0
    high: Float = 0.0
    low: Float = 0.0
    /// How likely rain is, in percent.
    chance: Float = 0.0
    code: Int = 0
    wind: Float = 0.0
    uv: Float = 0.0
    /// The place's own sunrise and sunset, `2026-06-30T05:01`.
    rise: Text = ""
    set: Text = ""
    icon: Text = "cloud"
}

/// What the sky is doing now, in the place's own day.
type Reading {
    at: Text = ""
    temperature: Float = 0.0
    apparent: Float = 0.0
    humidity: Float = 0.0
    precipitation: Float = 0.0
    code: Int = 0
    wind: Float = 0.0
    /// Where the wind comes from, in degrees.
    wind_from: Float = 0.0
    pressure: Float = 0.0
    uv: Float = 0.0
    day: Bool = true
    icon: Text = "cloud"
}

/// The air over a place. A pollutant the service omits stays absent, never a fabricated zero.
type Air {
    at: Text = ""
    /// The European AQI (0–20 good … above 100 extremely poor).
    european: Option<Float> = .none
    /// The United States AQI (0–500).
    us: Option<Float> = .none
    pm2_5: Option<Float> = .none
    pm10: Option<Float> = .none
    no2: Option<Float> = .none
    ozone: Option<Float> = .none
}

/// One place a person follows. What it is belongs to the workspace; what any device read of it is
/// the device's own ([`PlaceReading`]).
type Place (title: name) {
    name: Text
    /// The administrative region, when the search named one.
    region: Text = ""
    country: Text = ""
    country_code: Text = ""
    latitude: Float
    longitude: Float
    /// The place's IANA zone, or `auto` when the search named none.
    timezone: Text = "auto"

    search {
        sort name a_z
        hide latitude
        hide longitude
        hide timezone
    }
}

/// What this device last read of one place: the sky, the hours and the week ahead, the air, and
/// how the read stands. Two devices keep their own, so a slow service on one never holds the
/// person's other devices, and no one else in the workspace sees another's readings.
type PlaceReading {
    /// The place's name when this device read it: what the reading is called in its history.
    name: Text = ""
    /// The place this reading is of, by its id.
    place: Text = ""
    /// Whether this device has read the place: the fields below hold until it has.
    read: Bool = false
    /// What the sky is doing there now.
    current: Reading
    /// The hours ahead, as the device read them.
    hours: Collection<Hour>
    /// The week ahead.
    days: Collection<Day>
    /// The air over the place.
    air: Air
    /// Whether the air service answered: its reading stands even when this is false.
    has_air: Bool = false
    /// When this device last read the place.
    fetched: Option<Time> = .none
    /// The reading is older than the service's hour and no longer current.
    stale: Bool = false
    /// Why the last read failed, in words; empty while the place stands well.
    trouble: Text = ""
}

/// How far an update has got, and how the last one ended.
type Refresh {
    /// `running` while the device reads the places, and `done` once it has.
    state: Text = "done"
    /// The places this update has still to read, by their ids.
    waiting: List<Text>
    done: Int merge counter = 0
    total: Int = 0
    /// How many places the last update could not read.
    failed: Int merge counter = 0
    /// Why it could not start, in words; empty otherwise.
    note: Text = ""
}

/// One place a search found.
type Found {
    name: Text = ""
    latitude: Float = 0.0
    longitude: Float = 0.0
    country: Text = ""
    country_code: Text = ""
    region: Text = ""
    timezone: Text = "auto"
}

/// A person's own place search: what they typed, what the device found, and what it was asked
/// for. It is theirs alone, so two people looking for different places never see each other's
/// rows.
type Searching (readers: author) {
    /// What is in the field.
    wanted: Text = ""
    /// What the device was last asked for: it looks when the two differ.
    asked: Text = ""
    found: Collection<Found>
    /// Why the search could not be read, in words; empty otherwise.
    note: Text = ""
}

/// The places people follow together, the one a window shows first, and each person's search.
type Locations {
    places: Collection<Place>
    /// The place a window shows first; none for the first in the list.
    default: Option<Place> = .none
    /// Each person's own search here.
    searches: Collection<Searching>
    /// Today where the person is, `2026-06-30`: what a day of a forecast is named by.
    today: Text = ""
    /// The day after today, as `today` is.
    tomorrow: Text = ""
}

/// What this device read of the places, and how its last update went. A device keeps its own:
/// reading a forecast costs the service and a wait, and it is this computer's connection that
/// paid for it, never one another member waits on.
type Readings {
    /// One place's reading, this device's own.
    of: Collection<PlaceReading>
    refresh: Refresh
}

root weather: Locations

device root readings: Readings

default view weather_window for Locations

/// A place opens on its own page: what a search hit, a reference or another app shows when it
/// asks for the place rather than for Weather.
default view place_page for Place

/// How a place reads in a list of results: its name, its sky and its temperature.
row view place_row for Place

/// What this device read of `place`, or the empty reading a place has before it is read.
fn reading_of(r: Readings, place: Place) -> PlaceReading {
    if let found = r.of.find(reading -> reading.place == place.id()) {
        return found
    }
    PlaceReading(name: place.name, place: place.id())
}

/// What this device read of `place`, made the first time something writes it.
action reading_for(r: Readings, place: Place) -> PlaceReading {
    if let found = r.of.find(reading -> reading.place == place.id()) {
        return found
    }
    r.of.insert(PlaceReading(name: place.name, place: place.id()))
}

/// Saves a place a person follows. A place near one already saved is that place, not a second
/// entry, and a new place is the one windows show first.
// A place is added through the app's own search ("Add place"), which is the window's primary
// action; the action that writes one stays callable by scripts and agents.
action save_place(
    w: Locations,
    name: Text,
    latitude: Float,
    longitude: Float,
    region: Text = "",
    country: Text = "",
    country_code: Text = "",
    timezone: Text = "auto",
) -> Place (icon: .plus, label: "Save a place", did: "saved a place") {
    require name.trim() != "" "A place needs a name"
    require latitude >= -90.0 and latitude <= 90.0 "A place lies between latitudes -90 and 90"
    require longitude >= -180.0 and longitude <= 180.0 "A place lies between longitudes -180 and 180"
    if let had = w.places.find(p -> near(p, latitude, longitude)) {
        return had
    }
    let place = w.places.insert(
        Place(
            name: name.trim(),
            region: region.trim(),
            country: country.trim(),
            country_code: country_code.trim(),
            latitude: latitude,
            longitude: longitude,
            timezone: if timezone.trim() == "" { "auto" } else { timezone.trim() },
        ),
    )
    w.default = .some(place)
    place
}

/// Shows a place's forecast first whenever the app opens.
action use_as_default(w: Locations, place: Place) (
    icon: .pin,
    label: "Show first",
    did: "chose the place shown first",
) {
    w.default = .some(place)
}

/// Moves a place to the top of the list.
action move_to_top(w: Locations, place: Place) (icon: .arrow_up, label: "Move to top") {
    require w.places.index_of(place) != .some(0) "This place is already at the top"
    w.places.move(place, to: 0)
}

/// Renames a place the person follows.
action rename_place(w: Locations, place: Place, name: Text) (
    icon: .pencil,
    label: "Rename",
    did: "renamed a place",
) {
    require name.trim() != "" "A place needs a name"
    place.name = name.trim()
}

/// Removes a place from the list; what was read of it goes with it. Undo brings it back.
destructive action remove_place(w: Locations, place: Place) (
    icon: .trash_2,
    label: "Delete place",
    did: "deleted a place",
) {
    let was_default = w.default == .some(place)
    let mine = readings
    if let here = mine.of.find(reading -> reading.place == place.id()) { mine.of.remove(here) }
    w.places.remove(place)
    if was_default {
        w.default = .none
    }
}

/// Reads the forecasts again: every place, or the one `place` names. The window reads a few
/// places each second (`poll_weather`), so a slow service costs one place at a time and each
/// forecast lands as it is read.
action refresh(w: Locations, place: Option<Place> = .none) uses web, time (
    icon: .refresh,
    key: "r",
    label: "Refresh forecasts",
    did: "updated the forecasts",
) {
    require not w.places.is_empty() "There are no places to update"
    require readings.refresh.state != "running" "An update is already running"
    let mine = readings
    let places: List<Place> = if let one = place { [one] } else { w.places.map(p -> p) }
    for p in places {
        let here = reading_for(mine, p)
        here.trouble = ""
    }
    mine.refresh.waiting = places.map(p -> p.id())
    mine.refresh.state = "running"
    mine.refresh.done -= mine.refresh.done
    mine.refresh.total = places.len()
    // What the last update added up to goes back to nothing for this one.
    mine.refresh.failed -= mine.refresh.failed
    mine.refresh.note = ""
    read_next(w)
}

/// Stops an update that is running; the places it had not read keep what they had.
action stop_refresh(w: Locations) (icon: .x, label: "Stop updating") {
    let mine = readings
    mine.refresh.state = "done"
    mine.refresh.waiting = []
}

/// Updates a place whose last read failed, once the person asks again.
action retry_place(w: Locations, place: Place) uses web, time (
    icon: .refresh,
    label: "Try again",
    did: "asked for a forecast again",
) {
    let here = reading_for(readings, place)
    here.trouble = ""
    refresh(w, place: .some(place))
}

/// Reads one place an update has still to read, and says when the update has read them all.
// ---------------------------------------------------------------- reading the forecasts
action read_next(w: Locations) uses web, time {
    let mine = readings
    if mine.refresh.state != "running" {
        return
    }
    if let id = mine.refresh.waiting.first() {
        mine.refresh.waiting = mine.refresh.waiting.filter(x -> x != id)
        if let place = w.places.find(p -> p.id() == id) {
            let read = read_place(mine, place)
            mine.refresh.done += 1
            if not read {
                mine.refresh.failed += 1
            }
        }
    }
    if mine.refresh.waiting.is_empty() {
        mine.refresh.state = "done"
    }
}

/// Reads one place's forecast and air quality. One failing leaves the other's answer standing,
/// and a place the service did not answer keeps what it had and says why.
action read_place(r: Readings, place: Place) -> Bool uses web, time {
    let here = reading_for(r, place)
    let answer = web.get(forecast_address(place.latitude, place.longitude))
    if not answer.ok {
        here.trouble = refusal(answer)
        return false
    }
    if not apply_forecast(here, answer.text()) {
        here.trouble = "The answer wasn't a forecast"
        return false
    }
    here.has_air = false
    let air = web.get(air_address(place.latitude, place.longitude))
    if air.ok {
        if let reading = read_air(air.text()) {
            here.air.at = reading.at
            here.air.european = reading.european
            here.air.us = reading.us
            here.air.pm2_5 = reading.pm2_5
            here.air.pm10 = reading.pm10
            here.air.no2 = reading.no2
            here.air.ozone = reading.ozone
            here.has_air = true
        }
    }
    here.read = true
    here.trouble = ""
    here.fetched = .some(now)
    true
}

/// What a service's refusal says, as the place's row reads it.
fn refusal(answer: Response) -> Text {
    if answer.status == 0 {
        "Couldn't reach the weather service. Check your connection."
    } else if answer.status > 0 {
        std.say("The weather service refused the request (HTTP {answer.status})")
    } else {
        "The weather service refused the request"
    }
}

/// The forecast a service answered, written into this device's reading of the place: its
/// current reading, a day of hours and a week of days. False when the body is not a forecast
/// the service could have sent.
action apply_forecast(here: PlaceReading, body: Text) -> Bool {
    let root = match json.parse(body) {
        .ok(value) => value,
        .err(why) => {
            return false
        }
    }
    let current = field_of(root, "current")
    let hourly = field_of(root, "hourly")
    let daily = field_of(root, "daily")
    if let cur = current, hours = hourly, days = daily {
        let at = text_of(cur, "time")
        here.current.at = at
        here.current.temperature = number_at(cur, "temperature_2m")
        here.current.apparent = number_at(cur, "apparent_temperature")
        here.current.humidity = number_at(cur, "relative_humidity_2m")
        here.current.precipitation = number_at(cur, "precipitation")
        here.current.code = number_at(cur, "weather_code").to_int()
        here.current.wind = number_at(cur, "wind_speed_10m")
        here.current.wind_from = number_at(cur, "wind_direction_10m")
        here.current.pressure = number_at(cur, "pressure_msl")
        here.current.day = number_at(cur, "is_day") != 0.0
        here.current.icon = glyph(here.current.code, here.current.day)
        // The reading's own uv is today's highest: the service gives no current one.
        let uv = numbers_of(list_at(days, "uv_index_max"))
        here.current.uv = uv.first() ?? 0.0
        let hour_at = texts_of(list_at(hours, "time"))
        let hour_temp = numbers_of(list_at(hours, "temperature_2m"))
        let hour_chance = numbers_of(list_at(hours, "precipitation_probability"))
        let hour_code = counts_of(list_at(hours, "weather_code"))
        let hour_day = flags_of(list_at(hours, "is_day"))
        let start = start_at(hour_at, at)
        for old in here.hours.map(hour -> hour) {
            here.hours.remove(old)
        }
        for i in hour_at.map((time, i) -> i) {
            if i >= start and i < start + 24 {
                let code = hour_code.at(i) ?? 0
                let day = hour_day.at(i) ?? true
                here.hours.insert(
                    Hour(
                        at: hour_at.at(i) ?? "",
                        temperature: hour_temp.at(i) ?? 0.0,
                        chance: hour_chance.at(i) ?? 0.0,
                        code: code,
                        day: day,
                        icon: glyph(code, day),
                    ),
                )
            }
        }
        let day_date = texts_of(list_at(days, "time"))
        let day_high = numbers_of(list_at(days, "temperature_2m_max"))
        let day_low = numbers_of(list_at(days, "temperature_2m_min"))
        let day_chance = numbers_of(list_at(days, "precipitation_probability_max"))
        let day_code = counts_of(list_at(days, "weather_code"))
        let day_wind = numbers_of(list_at(days, "wind_speed_10m_max"))
        let day_uv = numbers_of(list_at(days, "uv_index_max"))
        let day_rise = texts_of(list_at(days, "sunrise"))
        let day_set = texts_of(list_at(days, "sunset"))
        for old in here.days.map(day -> day) {
            here.days.remove(old)
        }
        for i in day_date.map((date, i) -> i) {
            let code = day_code.at(i) ?? 0
            here.days.insert(
                Day(
                    date: day_date.at(i) ?? "",
                    weekday: weekday(day_date.at(i) ?? ""),
                    high: day_high.at(i) ?? 0.0,
                    low: day_low.at(i) ?? 0.0,
                    chance: day_chance.at(i) ?? 0.0,
                    code: code,
                    wind: day_wind.at(i) ?? 0.0,
                    uv: day_uv.at(i) ?? 0.0,
                    rise: day_rise.at(i) ?? "",
                    set: day_set.at(i) ?? "",
                    icon: glyph(code, true),
                ),
            )
        }
        true
    } else { false }
}

/// The air a service answered, or nothing when its body is no air-quality reading. A value it
/// omits stays absent, never a fabricated zero.
fn read_air(body: Text) -> Option<Air> {
    let root = match json.parse(body) {
        .ok(value) => value,
        .err(why) => {
            return .none
        }
    }
    if let cur = field_of(root, "current") {
        .some(
            Air(
                at: text_of(cur, "time"),
                european: optional_at(cur, "european_aqi"),
                us: optional_at(cur, "us_aqi"),
                pm2_5: optional_at(cur, "pm2_5"),
                pm10: optional_at(cur, "pm10"),
                no2: optional_at(cur, "nitrogen_dioxide"),
                ozone: optional_at(cur, "ozone"),
            ),
        )
    } else { .none }
}

/// The index at which the hours ahead begin: the first hour the place's own clock has reached.
fn start_at(times: List<Text>, at: Text) -> Int {
    let index = 0
    for time in times {
        if time >= at {
            return index
        }
        index = index + 1
    }
    0
}

/// The weekday a `YYYY-MM-DD` date falls on, counted from Monday as 0; 0 when the text is no
/// date. Sakamoto's algorithm: a month is shifted so March starts the year, which makes the leap
/// day the last day of a year of thirteen months.
fn weekday(date: Text) -> Int {
    let parts = date.split("-")
    let named = (parts.at(0) ?? "").to_int()
    let month = (parts.at(1) ?? "").to_int()
    let day = (parts.at(2) ?? "").to_int()
    if let year = named, month = month, day = day {
        if month >= 1 and month <= 12 and day >= 1 and day <= 31 {
            let shifts = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4]
            let counted = if month < 3 { year - 1 } else { year }
            let sunday_first = (counted + counted / 4 - counted / 100
                + counted / 400
                + (shifts.at(month - 1) ?? 0)
                + day)
                % 7
            // Sunday first becomes Monday first.
            return (sunday_first + 6) % 7
        } else {
            return 0
        }
    }
    0
}

/// The value at `key` of a JSON object, and nothing for anything else.
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

/// The number at `key` of a JSON object, or zero.
fn number_at(value: json.Value, key: Text) -> Float {
    number_of(field_of(value, key))
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

/// The number a value holds when it is a number, or nothing: a value the service omitted stays
/// absent.
fn optional_number(value: Option<json.Value>) -> Option<Float> {
    match value {
        .some(held) => match held {
            .number(number) => .some(number),
            .string(text) => text.to_float(),
            .object(fields) => .none,
            .array(items) => .none,
            .bool(flag) => .none,
            .null => .none,
        },
        .none => .none,
    }
}

/// The number at `key` of a JSON object, or nothing.
fn optional_at(value: json.Value, key: Text) -> Option<Float> {
    optional_number(field_of(value, key))
}

/// The array at `key` of a JSON object, or nothing.
fn list_at(value: json.Value, key: Text) -> List<json.Value> {
    match field_of(value, key) {
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

/// The numbers a JSON array holds, a value that is no number reading as zero.
fn numbers_of(items: List<json.Value>) -> List<Float> {
    items.map(item -> number_of(.some(item)))
}

/// The whole numbers a JSON array holds, a value that is no number reading as zero.
fn counts_of(items: List<json.Value>) -> List<Int> {
    items.map(item -> number_of(.some(item)).to_int())
}

/// The flags a JSON array holds, a value that is no number reading as false.
fn flags_of(items: List<json.Value>) -> List<Bool> {
    items.map(item -> number_of(.some(item)) != 0.0)
}

/// The texts a JSON array holds, a value that is no text reading as empty.
fn texts_of(items: List<json.Value>) -> List<Text> {
    items.map(
        item -> match item {
            .string(text) => text,
            .object(fields) => "",
            .array(inside) => "",
            .number(number) => "",
            .bool(flag) => "",
            .null => "",
        },
    )
}

/// A place search's answers, as the places a person may save.
fn found_places(items: List<json.Value>) -> List<Found> {
    items.map(
        item -> Found(
            name: text_of(item, "name"),
            latitude: number_at(item, "latitude"),
            longitude: number_at(item, "longitude"),
            country: text_of(item, "country"),
            country_code: text_of(item, "country_code"),
            region: text_of(item, "admin1"),
            timezone: if text_of(item, "timezone") == "" {
                "auto"
            } else { text_of(item, "timezone") },
        ),
    )
}

/// The places worth offering, in the service's order: Open-Meteo also matches a place's other
/// names ("Amsterdam" brings back a town now called "eMvelo"), and results that would read the
/// same show once.
fn refine(query: Text, places: List<Found>) -> List<Found> {
    let needle = query.trim().lower()
    let named = places.filter(place -> place.name.lower().contains(needle))
    let kept = if needle != "" and not named.is_empty() { named } else { places }
    let seen: List<Text> = []
    let out: List<Found> = []
    for place in kept {
        let key = "{place.name.lower()}/{place.region.lower()}/{place.country.lower()}"
        if not seen.contains(key) {
            seen = seen + [key]
            out = out + [place]
        }
    }
    out
}

/// The forecast address for a place, asked in metric and stored so: the app converts to the
/// units the person chose as it draws, so changing them never costs a fetch.
fn forecast_address(latitude: Float, longitude: Float) -> Text {
    "https://api.open-meteo.com/v1/forecast?latitude={place_number(latitude)}&longitude={place_number(longitude)}&current=temperature_2m,apparent_temperature,relative_humidity_2m,precipitation,weather_code,wind_speed_10m,wind_direction_10m,pressure_msl,is_day&hourly=temperature_2m,precipitation_probability,weather_code,is_day&daily=weather_code,temperature_2m_max,temperature_2m_min,precipitation_probability_max,sunrise,sunset,wind_speed_10m_max,uv_index_max&timezone=auto&forecast_days=7&temperature_unit=celsius&wind_speed_unit=kmh"
}

/// The air-quality address for a place: the two AQI scales and the pollutants that matter most.
fn air_address(latitude: Float, longitude: Float) -> Text {
    "https://air-quality-api.open-meteo.com/v1/air-quality?latitude={place_number(latitude)}&longitude={place_number(longitude)}&current=european_aqi,us_aqi,pm2_5,pm10,nitrogen_dioxide,ozone&timezone=auto"
}

/// A place search's address: up to eight answers, in the service's own order.
fn geocode_address(query: Text) -> Text {
    "https://geocoding-api.open-meteo.com/v1/search?name={query.url_encode()}&count=8&language=en&format=json"
}

/// A coordinate as the service reads it: four places after the point.
fn place_number(number: Float) -> Text {
    number.round(4).to_text()
}

/// Takes what the last read found: a place's reading and its hours and days land together, and
/// the search's rows beside the field. What an update reads is nobody's step of their own, so
/// landing it says nothing and offers no undo of its own. A second read of the same place
/// replaces the first.
// ---------------------------------------------------------------- what has landed
action poll_weather(w: Locations) uses time, web {
    let day = now.day().iso()
    if w.today != day {
        w.today = day
        w.tomorrow = (now + 1d).day().iso()
    }
    let mine = readings
    if mine.refresh.state == "running" {
        // A few places a second, side by side as a service likes: enough that a handful finishes
        // together, few enough that a large library never holds the window for long.
        read_next(w)
        if mine.refresh.state == "running" { read_next(w) }
        if mine.refresh.state == "running" { read_next(w) }
        if mine.refresh.state == "running" { read_next(w) }
    } else if mine.refresh.note == "" and w.places.any(p -> unread(mine, p)) {
        // A place nothing has been read of yet is read the first time the app runs here, unless
        // its last read failed: a service that refused stays refused until a person asks again.
        refresh(w)
    }
    // What a person is looking for: the device is asked once per query, so typing starts one read
    // rather than one per keystroke, and the last of them is the one that stands.
    if let reader = w.searches.first() {
        if reader.wanted.trim() != reader.asked {
            let query = reader.wanted.trim()
            reader.asked = query
            for old in reader.found.map(place -> place) {
                reader.found.remove(old)
            }
            reader.note = ""
            if query.len() >= 2 {
                let answer = web.get(geocode_address(query))
                if not answer.ok {
                    reader.note = refusal(answer)
                } else {
                    match json.parse(answer.text()) {
                        .err(why) => {
                            reader.note = std.say("The place search could not be read: {why}")
                        }
                        .ok(root) => {
                            let places = refine(query, found_places(list_at(root, "results")))
                            for place in places {
                                reader.found.insert(place)
                            }
                        }
                    }
                }
            }
        }
    }
    // A reading that has stood for hours says so, so the page never calls it current.
    for here in mine.of {
        let stale = if let read = here.fetched { (now - read).seconds() >= 7200 } else { false }
        if here.stale != stale {
            here.stale = stale
        }
    }
}

/// The forecasts are taken as they land, and a place nothing is known of is read once.
trigger watch_weather(w: Locations) every 1s: poll_weather(w)

/// Whether `place` stands within a twentieth of a degree of a point: about five kilometres.
fn near(place: Place, latitude: Float, longitude: Float) -> Bool {
    (place.latitude - latitude).abs() < 0.05 and (place.longitude - longitude).abs() < 0.05
}

/// Whether a place's reading is missing or old enough to fetch again: the service keeps to its
/// hour, and a reading of two hours is no longer the current one.
fn needs_update(r: Readings, place: Place, moment: Time) -> Bool {
    let here = reading_of(r, place)
    if here.trouble != "" {
        false
    } else if let read = here.fetched { (moment - read).seconds() >= 7200 } else { true }
}

/// Whether this device has read nothing of `place` yet, and its last read did not fail: what the
/// first run here reads.
fn unread(r: Readings, place: Place) -> Bool {
    let here = reading_of(r, place)
    not here.read and here.trouble == ""
}

/// Opening the window reads the places whose forecast is missing or no longer current. Nothing
/// is read while it stands, so opening a window again costs nothing.
opened action arrive(w: Locations) uses web, time {
    let mine = readings
    if mine.refresh.state != "running" and w.places.any(p -> needs_update(mine, p, now)) {
        refresh(w)
    }
}

/// Looks for places by name: the window asks the service once per query as it is typed.
// ---------------------------------------------------------------- looking for a place
action begin_search(w: Locations) -> Searching (icon: .search, label: "Look for a place") {
    let found = w.searches.first() ?? w.searches.insert(Searching())
    found.wanted = ""
    found.asked = ""
    for old in found.found.map(place -> place) {
        found.found.remove(old)
    }
    found.note = ""
    found
}

/// Empties the field and stops looking.
action end_search(w: Locations, reader: Searching) {
    reader.wanted = ""
    reader.asked = ""
    for old in reader.found.map(place -> place) {
        reader.found.remove(old)
    }
    reader.note = ""
}

/// Saves the place a search found, by its place in the list of what it found, and shows it.
action add_found(w: Locations, reader: Searching, at: Int) -> Place {
    let found = reader.found.at(at) ?? Found()
    save_place(
        w,
        name: found.name,
        latitude: found.latitude,
        longitude: found.longitude,
        region: found.region,
        country: found.country,
        country_code: found.country_code,
        timezone: found.timezone,
    )
}

/// Takes the row a person picked from the search: saves the place and empties the search.
action accept_found(w: Locations, reader: Searching, choice: Text) {
    let at = choice.to_int() ?? 0
    add_found(w, reader, at)
    end_search(w, reader)
}

/// The display units the person chose in Settings: the readings are kept in metric and converted
/// as they are drawn, so changing them never costs a fetch.
// ---------------------------------------------------------------- units and words
fn degrees(celsius: Float, imperial: Bool) -> Int {
    if imperial { (celsius * 9.0 / 5.0 + 32.0).round() } else { celsius.round() }
}

/// A temperature with its degree sign.
fn degree_words(celsius: Float, imperial: Bool) -> Text {
    "{degrees(celsius, imperial)}°"
}

fn speed(kmh: Float, imperial: Bool) -> Int {
    if imperial { (kmh * 0.621371).round() } else { kmh.round() }
}

fn speed_unit(imperial: Bool) -> Text {
    if imperial { "mph" } else { "km/h" }
}

/// Rain in the unit the person reads: tenths of a millimetre, hundredths of an inch.
fn rain_words(millimetres: Float, imperial: Bool) -> Text {
    if imperial { (millimetres / 25.4).round(2).to_text() } else { millimetres.round(1).to_text() }
}

fn rain_unit(imperial: Bool) -> Text {
    if imperial { "in" } else { "mm" }
}

/// Where the wind comes from, by the sixteen points of a compass.
fn compass(degrees_from: Float) -> Text {
    let points = [
        "N",
        "NNE",
        "NE",
        "ENE",
        "E",
        "ESE",
        "SE",
        "SSE",
        "S",
        "SSW",
        "SW",
        "WSW",
        "W",
        "WNW",
        "NW",
        "NNW",
    ]
    let turn = (degrees_from / 22.5).round()
    points.at((turn % 16 + 16) % 16) ?? "N"
}

/// The glyph a weather code shows, by the icon set's name: the sun and the moon stand for a
/// clear sky, so a place's row reads its day at a glance.
fn glyph(code: Int, day: Bool) -> Text {
    if code == 0 or code == 1 {
        if day { "sun" } else { "moon" }
    } else if code == 45 or code == 48 {
        "cloud"
    } else if code == 51 or code == 53 or code == 55 {
        "cloud_rain"
    } else if code == 56 or code == 57 or code == 66 or code == 67 {
        "cloud_snow"
    } else if code == 61 or code == 63 or code == 65 or code == 80 or code == 81 or code == 82 {
        "cloud_rain"
    } else if code == 71 or code == 73 or code == 75 or code == 77 or code == 85 or code == 86 {
        "cloud_snow"
    } else if code == 95 or code == 96 or code == 99 { "zap" } else { "cloud_sun" }
}

/// What the sky is doing, in the words Open-Meteo's codes stand for.
fn condition(code: Int) -> Text {
    if code == 0 {
        "Clear sky"
    } else if code == 1 {
        "Mainly clear"
    } else if code == 2 {
        "Partly cloudy"
    } else if code == 3 {
        "Overcast"
    } else if code == 45 {
        "Fog"
    } else if code == 48 {
        "Rime fog"
    } else if code == 51 {
        "Light drizzle"
    } else if code == 53 {
        "Drizzle"
    } else if code == 55 {
        "Dense drizzle"
    } else if code == 56 {
        "Freezing drizzle"
    } else if code == 57 {
        "Dense freezing drizzle"
    } else if code == 61 {
        "Slight rain"
    } else if code == 63 {
        "Rain"
    } else if code == 65 {
        "Heavy rain"
    } else if code == 66 {
        "Freezing rain"
    } else if code == 67 {
        "Heavy freezing rain"
    } else if code == 71 {
        "Slight snow"
    } else if code == 73 {
        "Snow"
    } else if code == 75 {
        "Heavy snow"
    } else if code == 77 {
        "Snow grains"
    } else if code == 80 {
        "Light showers"
    } else if code == 81 {
        "Showers"
    } else if code == 82 {
        "Violent showers"
    } else if code == 85 {
        "Slight snow showers"
    } else if code == 86 {
        "Snow showers"
    } else if code == 95 {
        "Thunderstorm"
    } else if code == 96 {
        "Thunderstorm with hail"
    } else if code == 99 { "Severe thunderstorm" } else { "Weather" }
}

/// How strong the sun burns, in the words a person reads.
fn uv_words(uv: Float) -> Text {
    let n = uv.round()
    if n <= 2 {
        "Low"
    } else if n <= 5 {
        "Moderate"
    } else if n <= 7 { "High" } else if n <= 10 { "Very high" } else { "Extreme" }
}

/// The band the European air quality index falls in.
fn aqi_band(aqi: Float) -> Text {
    let n = aqi.round()
    if n <= 20 {
        "Good"
    } else if n <= 40 {
        "Fair"
    } else if n <= 60 {
        "Moderate"
    } else if n <= 80 { "Poor" } else if n <= 100 { "Very poor" } else { "Extremely poor" }
}

/// A pollutant that may be absent, as its tile says it.
fn measured(value: Option<Float>, unit: Text) -> Text {
    if let n = value { "{n.round()} {unit}" } else { "—" }
}

/// A weekday's short name, as the days of the week ahead are listed.
fn weekday_name(weekday: Int) -> Text {
    if weekday == 0 {
        "Mon"
    } else if weekday == 1 {
        "Tue"
    } else if weekday == 2 {
        "Wed"
    } else if weekday == 3 {
        "Thu"
    } else if weekday == 4 { "Fri" } else if weekday == 5 { "Sat" } else { "Sun" }
}

/// A day's own name: today, tomorrow, or the weekday it falls on.
fn day_name(day: Day, today: Text, tomorrow: Text) -> Text {
    if day.date == today {
        "Today"
    } else if day.date == tomorrow { "Tomorrow" } else { weekday_name(day.weekday) }
}

/// An ISO local time, `2026-06-30T05:01`, as the clock it names, `05:01`.
fn hhmm(iso: Text) -> Text {
    if iso.len() >= 16 { iso.slice(11, 16) } else { "--:--" }
}

/// The minutes after midnight a `HH:MM` names, when it names one.
fn minutes_of(clock: Text) -> Option<Int> {
    let parts = clock.split(":")
    if let hour = (parts.at(0) ?? "").to_int(), minute = (parts.at(1) ?? "").to_int() {
        if hour >= 0 and hour < 24 and minute >= 0 and minute < 60 {
            .some(hour * 60 + minute)
        } else { .none }
    } else { .none }
}

/// How far through its daylight a place is, from 0 at sunrise to 1 at sunset.
fn daylight_fraction(now_at: Text, day: Day) -> Option<Float> {
    if let moment = minutes_of(hhmm(now_at)),
        rise = minutes_of(hhmm(day.rise)),
        set = minutes_of(hhmm(day.set)) {
        if set > rise {
            .some(((moment - rise).to_float() / (set - rise).to_float()).clamp(0.0, 1.0))
        } else { .none }
    } else { .none }
}

/// What a place is called under its name: its region and country, without repeating either.
fn subtitle(place: Place) -> Text {
    if place.region != "" and place.region != place.name {
        "{place.region}, {place.country}"
    } else { place.country }
}

/// A place a search found, as one line: what tells two places of the same name apart.
fn found_line(found: Found) -> Text {
    if found.region != "" and found.region != found.name {
        "{found.name}, {found.region}, {found.country}"
    } else if found.country != "" { "{found.name}, {found.country}" } else { found.name }
}

/// The place a window shows: what the person chose, else the one shown first, else the first.
fn shown_place(w: Locations, shown: Option<Place>) -> Option<Place> {
    shown ?? w.default ?? w.places.first()
}

/// The key the list marks as shown.
fn shown_key(w: Locations, shown: Option<Place>) -> Text {
    if let place = shown_place(w, shown) { place.id() } else { "" }
}

/// What the field under the search says when there is nothing to choose from.
fn search_words(reader: Searching) -> Text {
    if reader.wanted.trim().len() < 2 {
        "Type two letters or more"
    } else if reader.note != "" {
        reader.note
    } else if reader.found.is_empty() { "No places found" } else { "" }
}

/// The tile of one hour: its clock, its sky, its temperature and how likely rain is.
// ---------------------------------------------------------------- the window
view hour_tile(hour: Hour, imperial: Bool, now_at: Text) {
    stack(gap: .tight, align: .center, min: 3.5rem) {
        ui.Text(
            if hour.at == now_at { "Now" } else { hhmm(hour.at) },
            role: .caption,
            secondary: true,
        )
        ui.Icon(hour.icon, description: condition(hour.code))
        ui.Text(degree_words(hour.temperature, imperial), role: .body, numeric: true)
        if hour.chance > 0.0 {
            row(gap: .tight, align: .center) {
                ui.Icon(.droplet, size: .small, secondary: true)
                ui.Text("{hour.chance.round()}%", role: .caption, secondary: true)
            }
        }
    }
}

/// The hours ahead as a strip that scrolls sideways, one column an hour.
view hours_strip(here: PlaceReading, imperial: Bool, now_at: Text) {
    if here.hours.is_empty() {
        ui.Text("No hour-by-hour forecast yet.", role: .caption, secondary: true)
    } else {
        scroll(axis: .horizontal) {
            row(gap: .related) {
                for hour in here.hours {
                    hour_tile(hour, imperial, now_at)
                }
            }
        }
    }
}

/// One day of the week ahead: its name, its sky, how likely rain is, and the span of its
/// temperatures along the week's own scale, so the days are read against each other.
view day_row(day: Day, low: Float, high: Float, imperial: Bool, today: Text, tomorrow: Text) {
    let coldest = degrees(low, imperial)
    let warmest = degrees(high, imperial)
    let span = (high - low).max(0.1)
    let from = ((day.low - low) / span).clamp(0.0, 1.0)
    let width = ((day.high - low) / span - from).clamp(0.02, 1.0)
    // The day's name, its sky and its span of temperatures stand on one line where they fit; on
    // a phone the span and the highs wrap under the name rather than reaching past the row.
    row(gap: .related, align: .center, wrap: true) {
        ui.Text(day_name(day, today, tomorrow), role: .body, min: 5rem)
        ui.Icon(day.icon, description: condition(day.code))
        ui.Text(condition(day.code), role: .body, secondary: true, truncate)
        ui.Text(
            "UV {day.uv.round()}. Wind {day.wind.round()}.",
            role: .caption,
            secondary: true,
            truncate,
        )
        if day.chance > 0.0 {
            ui.Text("{day.chance.round()}%", role: .caption, secondary: true)
        }
        spacer()
        ui.Text("{degrees(day.low, imperial)}°", role: .body, secondary: true, numeric: true)
        ui.Drawing(
            [
                .rect(x: 0.0, y: 0.4, w: 1.0, h: 0.2, round: 1.0, fill: .track),
                .rect(x: from, y: 0.4, w: width, h: 0.2, round: 1.0, fill: .accent),
            ],
            meaning: .chart,
            label: "{coldest} to {warmest} degrees",
            width: 96.0,
            height: 8.0,
        )
        ui.Text("{degrees(day.high, imperial)}°", role: .body, numeric: true)
    }
}

/// The hero: the sky, the temperature, what it is doing, the day's span, where this is, and how
/// old the reading is.
view hero(place: Place, here: PlaceReading, imperial: Bool) {
    let reading = here.current
    row(gap: .group, align: .center) {
        ui.Icon(reading.icon, size: .large, description: condition(reading.code))
        stack(gap: .tight) {
            ui.Text(degree_words(reading.temperature, imperial), role: .display, numeric: true)
            ui.Text(condition(reading.code), role: .heading)
            if let today = here.days.first() {
                ui.Text(
                    "High {degrees(today.high, imperial)}° · low {degrees(today.low, imperial)}°",
                    role: .body,
                    secondary: true,
                )
            }
            ui.Text(subtitle(place), role: .caption, secondary: true)
            row(gap: .tight, align: .center) {
                if let fetched = here.fetched {
                    ui.Time(fetched, role: .caption, past: true, secondary: true)
                }
                if here.stale {
                    ui.Icon(.triangle_alert, size: .small, tone: .warning)
                    ui.Text("Out of date", role: .caption)
                }
            }
        }
        spacer()
        ui.Clock(
            zone: if place.timezone == "auto" { .none } else { .some(place.timezone) },
            facts: true,
            secondary: true,
        )
    }
}

/// The readings beside the hero's temperature.
view readings(reading: Reading, imperial: Bool) {
    ui.StatGroup {
        ui.StatTile("Feels like", degree_words(reading.apparent, imperial))
        ui.StatTile(
            "Wind",
            "{speed(reading.wind, imperial)}",
            unit: speed_unit(imperial),
            note: compass(reading.wind_from),
        )
        ui.StatTile("Humidity", "{reading.humidity.round()}", unit: "%")
        ui.StatTile("Rain", rain_words(reading.precipitation, imperial), unit: rain_unit(imperial))
        ui.StatTile("Pressure", "{reading.pressure.round()}", unit: "hPa")
        ui.StatTile("UV index", "{reading.uv.round()}", note: uv_words(reading.uv))
    }
}

/// The air over the place: the index and the pollutants, each as the service reads it.
view air_quality(air: Air) {
    let any = air.european != .none
        or air.us != .none
        or air.pm2_5 != .none
        or air.pm10 != .none
        or air.no2 != .none
        or air.ozone != .none
    if any {
        ui.StatGroup {
            if let european = air.european {
                ui.StatTile(
                    "Air quality",
                    "{european.round()}",
                    note: aqi_band(european),
                    tone: if european > 60.0 { .warning } else { .neutral },
                )
            }
            if let us = air.us {
                ui.StatTile("US AQI", "{us.round()}")
            }
            ui.StatTile("PM2.5", measured(air.pm2_5, "µg/m³"))
            ui.StatTile("PM10", measured(air.pm10, "µg/m³"))
            ui.StatTile("Nitrogen dioxide", measured(air.no2, "µg/m³"))
            ui.StatTile("Ozone", measured(air.ozone, "µg/m³"))
        }
    } else {
        ui.Text("No air-quality reading for this place.", role: .caption, secondary: true)
    }
}

/// How far an update has got, over the forecast.
view update_bar(w: Locations) {
    let job = readings.refresh
    if job.state == "running" {
        stack(padding: .group, gap: .tight) {
            ui.Text(
                std.say("Updating {job.done} of {job.total} places"),
                role: .caption,
                secondary: true,
            )
            ui.ProgressBar(
                value: if job.total == 0 {
                    0.0
                } else {
                    job.done.to_float() / job.total.to_float()
                },
                label: "Updating the forecasts",
            )
            row {
                spacer()
                ui.Button("Stop", icon: .x) {
                    on press: stop_refresh(w)
                }
            }
        }
    } else if job.note != "" {
        ui.Banner(job.note, kind: .error, icon: .triangle_alert)
    } else if job.failed > 0 {
        ui.Banner(
            if job.failed == 1 {
                "1 place could not be updated"
            } else {
                "{job.failed} places could not be updated"
            },
            message: "What was read before still stands; try again later.",
            kind: .warning,
            icon: .triangle_alert,
            action: "Try again",
        ) {
            on act: refresh(w)
        }
    }
}

/// One place's forecast: its hero, its readings, the hours, the week and the air, or what stands
/// in their place while the device reads it. The place's own page — what the window shows for the
/// place a person chose, and what opening the place on its own shows, wherever it was opened from.
view place_page(place: Place) {
    let w = root Locations
    let imperial = settings.locale.measurement == .Imperial
    let here = reading_of(readings, place)
    ui.Scroll {
        page {
            stack(gap: .group) {
                update_bar(w)
                if here.trouble != "" {
                    ui.Banner(
                        "Couldn't update this forecast",
                        message: here.trouble,
                        kind: .warning,
                        icon: .cloud_off,
                        action: "Try again",
                    ) {
                        on act: retry_place(w, place)
                    }
                }
                if here.read {
                    hero(place, here, imperial)
                    readings(here.current, imperial)
                    if let today = here.days.first() {
                        if let fraction = daylight_fraction(here.current.at, today) {
                            ui.BarGauge(
                                fraction,
                                label: "Daylight",
                                readout: "{hhmm(today.rise)} – {hhmm(today.set)}",
                                neutral: true,
                                compact: true,
                            )
                        }
                    }
                    ui.Section("Next 24 hours") {
                        hours_strip(here, imperial, here.current.at)
                    }
                    ui.Section("The week ahead") {
                        let lows = here.days.map(d -> d.low).sort_by(x -> x)
                        let highs = here.days.map(d -> d.high).sort_by(x -> x)
                        let low = lows.first() ?? 0.0
                        let high = highs.last() ?? 1.0
                        stack(gap: .related) {
                            for day in here.days {
                                day_row(day, low, high, imperial, w.today, w.tomorrow)
                            }
                        }
                    }
                    if here.has_air {
                        ui.Section("Air quality") {
                            air_quality(here.air)
                        }
                    }
                } else if here.trouble == "" {
                    stack(gap: .group) {
                        ui.Skeleton(shape: .page)
                        ui.Text("Reading the forecast…", role: .caption, secondary: true)
                    }
                }
            }
        }
    }
}

/// How a place reads in a list of results: its name, what the sky is doing there and how warm it
/// is, or where it is while this device has not read it yet.
view place_row(p: Place) {
    let here = reading_of(readings, p)
    let imperial = settings.locale.measurement == .Imperial
    ui.ListRow(
        title: p.name,
        secondary: if here.read { condition(here.current.code) } else { subtitle(p) },
        item: p,
        icon: if here.read { here.current.icon } else { .map_pin },
        value: if here.read {
            degree_words(here.current.temperature, imperial)
        } else { "" },
    )
}

/// The places a person follows, in their order, each with its sky, and the choice the list
/// reports.
view places_side(w: Locations, shown: Option<Place>, chosen: State<Option<Place>>) {
    ui.NavList(selected: shown_key(w, shown), title: "Places") {
        for place in w.places {
            let here = reading_of(readings, place)
            if here.read {
                ui.NavItem(place.id(), place.name, icon: here.current.icon, item: place) {
                    on select: set(chosen, place)
                }
            } else {
                ui.NavItem(place.id(), place.name, icon: .map_pin, item: place) {
                    on select: set(chosen, place)
                }
            }
        }
    }
}

/// The window: the places a person follows, the one they are looking at, and the search that
/// saves another.
view weather_window(w: Locations) {
    state chosen: Option<Place> = .none
    state adding: Bool = false
    // A place a search hit, a reference or another app asks the window to show opens where the
    // window stands: the place it names is the one the window draws.
    on show(p: Place): set(chosen, .some(p))
    let place = shown_place(w, chosen)
    ui.NavShell(content_kind: .page, sidebar_title: "Weather", title: place?.name ?? "Weather") {
        actions: {
            ui.Button("Add place", icon: .plus, role: .primary) {
                on press: set(adding, true).then(begin_search(w))
            }
            // What stands on the place the window shows: its forecast read again, its place in
            // the window kept first, and its way out of the list. The bar keeps three in its row
            // and the rest in its More menu.
            if let shown = place {
                ui.Button("Refresh", icon: .refresh) {
                    on press: refresh(w, place: .some(shown))
                }
                ui.Button("Show first", icon: .pin) {
                    on press: use_as_default(w, shown)
                }
                ui.Button("Rename", icon: .pencil) {
                    on press: rename_place(w, shown)
                }
                ui.Button("Delete place", icon: .trash_2, role: .destructive) {
                    on press: remove_place(w, shown)
                }
            }
        }
        sidebar: {
            places_side(w, place, chosen)
        }
        content: {
            if w.places.is_empty() {
                ui.EmptyState(
                    "No places yet",
                    message: "Follow a city or town to see its forecast here.",
                    icon: .cloud_sun,
                    action: "Add place",
                ) {
                    on act: set(adding, true).then(begin_search(w))
                }
            } else if let shown = place {
                place_page(shown)
            } else {
                ui.EmptyState("Choose a place", icon: .map_pin)
            }
        }
    }
    if let reader = w.searches.first() {
        add_place_dialog(w, reader, adding)
    }
}

/// The search over the window: the field, what the device found, and the place a press saves.
view add_place_dialog(w: Locations, reader: Searching, adding: State<Bool>) {
    ui.ModalDialog(
        title: "Add a place",
        description: "Search for a city or town by name",
        open: adding,
        secondary: "Cancel",
    ) {
        on dismiss: set(adding, false).then(end_search(w, reader))
        on act_secondary: set(adding, false).then(end_search(w, reader))
        stack(gap: .tight) {
            ui.Autocomplete(
                bind: reader.wanted,
                placeholder: "A city or town",
                autofocus: true,
                open_list: true,
                filter: false,
                max_visible: 6,
                status: search_words(reader),
            ) {
                on accept(choice): accept_found(w, reader, choice)
                for at in reader.found.map((found, at) -> at) {
                    if let found = reader.found.at(at) {
                        ui.Suggestion("{at}", label: found_line(found), icon: .map_pin)
                    }
                }
            }
        }
    }
}

/// What a person first sees: three places, a week apart from each other, so a capture shows
/// the app doing real work.
// ---------------------------------------------------------------- what a person starts with
scenario "a week in three places" -> Locations {
    let w = root Locations
    let amsterdam = w.places.insert(
        Place(
            name: "Amsterdam",
            region: "North Holland",
            country: "Netherlands",
            country_code: "NL",
            latitude: 52.374,
            longitude: 4.8897,
            timezone: "Europe/Amsterdam",
        ),
    )
    let here_a = readings.of.insert(PlaceReading(name: amsterdam.name, place: amsterdam.id()))
    here_a.current.at = "2026-11-24T09:00"
    here_a.current.temperature = 7.4
    here_a.current.apparent = 4.8
    here_a.current.humidity = 82.0
    here_a.current.precipitation = 0.4
    here_a.current.code = 61
    here_a.current.wind = 18.0
    here_a.current.wind_from = 210.0
    here_a.current.pressure = 1012.0
    here_a.current.uv = 1.0
    here_a.current.day = true
    here_a.current.icon = "cloud_rain"
    here_a.read = true
    here_a.fetched = .some(2026-11-24T07:20Z)
    for hour in [
        Hour(
            at: "2026-11-24T09:00",
            temperature: 7.4,
            chance: 55.0,
            code: 61,
            day: true,
            icon: "cloud_rain",
        ),
        Hour(
            at: "2026-11-24T10:00",
            temperature: 8.0,
            chance: 40.0,
            code: 3,
            day: true,
            icon: "cloud",
        ),
        Hour(
            at: "2026-11-24T11:00",
            temperature: 8.6,
            chance: 25.0,
            code: 3,
            day: true,
            icon: "cloud",
        ),
        Hour(
            at: "2026-11-24T12:00",
            temperature: 9.1,
            chance: 15.0,
            code: 2,
            day: true,
            icon: "cloud_sun",
        ),
        Hour(
            at: "2026-11-24T13:00",
            temperature: 9.4,
            chance: 10.0,
            code: 2,
            day: true,
            icon: "cloud_sun",
        ),
        Hour(
            at: "2026-11-24T14:00",
            temperature: 9.2,
            chance: 10.0,
            code: 1,
            day: true,
            icon: "sun",
        ),
        Hour(
            at: "2026-11-24T15:00",
            temperature: 8.4,
            chance: 20.0,
            code: 2,
            day: true,
            icon: "cloud_sun",
        ),
        Hour(
            at: "2026-11-24T16:00",
            temperature: 7.1,
            chance: 35.0,
            code: 3,
            day: true,
            icon: "cloud",
        ),
        Hour(
            at: "2026-11-24T17:00",
            temperature: 6.3,
            chance: 45.0,
            code: 61,
            day: false,
            icon: "cloud_rain",
        ),
        Hour(
            at: "2026-11-24T18:00",
            temperature: 5.8,
            chance: 60.0,
            code: 63,
            day: false,
            icon: "cloud_rain",
        ),
    ] {
        here_a.hours.insert(hour)
    }
    for day in [
        Day(
            date: "2026-11-24",
            weekday: 1,
            high: 9.4,
            low: 5.2,
            chance: 60.0,
            code: 61,
            wind: 24.0,
            uv: 1.0,
            rise: "2026-11-24T08:09",
            set: "2026-11-24T16:38",
            icon: "cloud_rain",
        ),
        Day(
            date: "2026-11-25",
            weekday: 2,
            high: 8.1,
            low: 4.4,
            chance: 35.0,
            code: 3,
            wind: 20.0,
            uv: 1.0,
            rise: "2026-11-25T08:11",
            set: "2026-11-25T16:36",
            icon: "cloud",
        ),
        Day(
            date: "2026-11-26",
            weekday: 3,
            high: 6.7,
            low: 3.0,
            chance: 70.0,
            code: 63,
            wind: 34.0,
            uv: 0.0,
            rise: "2026-11-26T08:13",
            set: "2026-11-26T16:35",
            icon: "cloud_rain",
        ),
        Day(
            date: "2026-11-27",
            weekday: 4,
            high: 5.2,
            low: 1.8,
            chance: 20.0,
            code: 2,
            wind: 16.0,
            uv: 1.0,
            rise: "2026-11-27T08:15",
            set: "2026-11-27T16:34",
            icon: "cloud_sun",
        ),
    ] {
        here_a.days.insert(day)
    }
    here_a.air.at = "2026-11-24T09:00"
    here_a.air.european = 32.0
    here_a.air.us = 41.0
    here_a.air.pm2_5 = 8.4
    here_a.air.pm10 = 14.2
    here_a.air.no2 = 18.0
    here_a.has_air = true
    w.default = .some(amsterdam)
    let reykjavik = w.places.insert(
        Place(
            name: "Reykjavík",
            region: "Capital Region",
            country: "Iceland",
            country_code: "IS",
            latitude: 64.1466,
            longitude: -21.9426,
            timezone: "Atlantic/Reykjavik",
        ),
    )
    let here_r = readings.of.insert(PlaceReading(name: reykjavik.name, place: reykjavik.id()))
    here_r.current.at = "2026-11-24T08:00"
    here_r.current.temperature = -1.6
    here_r.current.apparent = -7.0
    here_r.current.humidity = 74.0
    here_r.current.code = 71
    here_r.current.wind = 42.0
    here_r.current.wind_from = 20.0
    here_r.current.pressure = 998.0
    here_r.current.day = false
    here_r.current.icon = "moon"
    here_r.read = true
    here_r.fetched = .some(2026-11-24T07:20Z)
    here_r.days.insert(
        Day(
            date: "2026-11-24",
            weekday: 1,
            high: 0.4,
            low: -4.1,
            chance: 80.0,
            code: 71,
            wind: 48.0,
            uv: 0.0,
            rise: "2026-11-24T10:24",
            set: "2026-11-24T15:58",
            icon: "cloud_snow",
        ),
    )
    w.today = "2026-11-24"
    w.tomorrow = "2026-11-25"
    w
}
