app so.tilo.weather "Weather" (
    icon: .cloud_sun,
    category: .utility,
    tagline: "Forecasts for the places you follow",
    keywords: ["weather", "forecast", "temperature", "rain", "wind", "snow", "wetter", "vorhersage", "temperatur", "regen", "schnee"],
    splash_budget: 250,
    web: ["api.open-meteo.com", "geocoding-api.open-meteo.com", "air-quality-api.open-meteo.com"],
)

use so.tilo.settings as settings

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

/// One place a person follows, and what this device last read of it.
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
    /// Whether this device has read the place: what the readings below hold until it has.
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

    search {
        sort name a_z
        hide latitude
        hide longitude
        hide timezone
        hide fetched
        hide stale
        hide trouble
    }
}

/// How far an update has got, and how the last one ended.
type Refresh {
    /// The job the device runs, by its id; empty while nothing runs.
    job: Text = ""
    /// `running` while the device reads the places, and `done` once it has.
    state: Text = "done"
    done: Int = 0
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

/// A person's own place search: what they typed, what the device found, and the job it runs.
/// It is theirs alone, so two people looking for different places never see each other's rows.
type Searching (readers: author) {
    /// What is in the field.
    wanted: Text = ""
    /// What the device was last asked for: a job starts when the two differ.
    asked: Text = ""
    /// The search job the device runs, by its id; empty while nothing runs.
    job: Text = ""
    /// The device is looking now.
    searching: Bool = false
    found: Collection<Found>
    /// Why the search could not be read, in words; empty otherwise.
    note: Text = ""
}

/// The places a person follows, the one a window shows first, and how each read stands.
type Locations {
    places: Collection<Place>
    /// The place a window shows first; none for the first in the list.
    default: Option<Place> = .none
    refresh: Refresh
    /// Each person's own search here.
    searches: Collection<Searching>
    /// Today where the person is, `2026-06-30`: what a day of a forecast is named by.
    today: Text = ""
    /// The day after today, as `today` is.
    tomorrow: Text = ""
}

root weather: Locations

default view weather_window for Locations

/// The places one update is asked for.
// ---------------------------------------------------------------- what the device reads
type Ask {
    places: List<AskPlace>
}

type AskPlace {
    place_id: Text
    latitude: Float
    longitude: Float
}

/// The search one job is asked for.
type SearchAsk {
    query: Text
}

/// The job a take names.
type TakeAsk {
    job: Text
}

/// How far a job has got.
type Job {
    state: Text
    done: Int
    total: Int
}

/// What a job finished since it was last taken.
type Taken {
    /// Each place's answer, as its own JSON text: a list of records is a list of resources in
    /// this language, and JSON text makes none, so each is read into its record here.
    answers: List<Text>
    /// Each place a search found, as its own JSON text.
    places: List<Text>
    /// Why a search could not be read, in the engine's words.
    error: Text = ""
}

/// One place's answer: its reading, the hours and days beside it as parallel lists, and its
/// air reading.
type Answer {
    /// The place the answer is about, by its id; not `id`, which every record's is its own.
    place_id: Text = ""
    ok: Bool = false
    reason: Text = ""
    status: Int = 0
    message: Text = ""
    /// When the device read it, in epoch seconds.
    fetched: Int = 0
    time: Text = ""
    temperature: Float = 0.0
    apparent: Float = 0.0
    humidity: Float = 0.0
    precipitation: Float = 0.0
    code: Int = 0
    wind: Float = 0.0
    wind_from: Float = 0.0
    pressure: Float = 0.0
    uv: Float = 0.0
    is_day: Bool = true
    hour_at: List<Text>
    hour_temp: List<Float>
    hour_chance: List<Float>
    hour_code: List<Int>
    hour_day: List<Bool>
    day_date: List<Text>
    day_weekday: List<Int>
    day_high: List<Float>
    day_low: List<Float>
    day_chance: List<Float>
    day_code: List<Int>
    day_wind: List<Float>
    day_uv: List<Float>
    day_rise: List<Text>
    day_set: List<Text>
    air_ok: Bool = false
    air_at: Text = ""
    air_european: Option<Float> = .none
    air_us: Option<Float> = .none
    air_pm2_5: Option<Float> = .none
    air_pm10: Option<Float> = .none
    air_no2: Option<Float> = .none
    air_ozone: Option<Float> = .none
}

/// Saves a place a person follows. A place near one already saved is that place, not a second
/// entry, and a new place is the one windows show first.
// ---------------------------------------------------------------- the places
primary action save_place(
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

/// Removes a place from the list; what was read of it goes with it. Undo brings it back.
destructive action remove_place(w: Locations, place: Place) (
    icon: .trash_2,
    label: "Remove place",
    did: "removed a place",
) {
    let was_default = w.default == .some(place)
    w.places.remove(place)
    if was_default {
        w.default = .none
    }
}

/// Reads the forecasts again: every place, or the one `place` names. The device fetches them
/// away from the frame and the window takes each answer as it lands (`poll_weather`), so a slow
/// service never holds the window.
action refresh(w: Locations, place: Option<Place> = .none) (
    icon: .refresh,
    key: "r",
    label: "Refresh forecasts",
    did: "updated the forecasts",
) {
    require not w.places.is_empty() "There are no places to update"
    require w.refresh.job == "" "An update is already running"
    let places: List<Place> = if let one = place { [one] } else { w.places.map(p -> p) }
    for p in places {
        p.trouble = ""
    }
    match ask_update(places) {
        .ok(job) => {
            w.refresh.job = job
            w.refresh.state = "running"
            w.refresh.done = 0
            w.refresh.total = places.len()
            // What the last update added up to goes back to nothing for this one.
            w.refresh.failed -= w.refresh.failed
            w.refresh.note = ""
        }
        .err(why) => {
            w.refresh.state = "done"
            w.refresh.job = ""
            w.refresh.note = std.say("Couldn't start updating: {why}")
        }
    }
}

/// Stops an update that is running.
action stop_refresh(w: Locations) (icon: .x, label: "Stop updating") {
    if w.refresh.job != "" { engine.cancel(w.refresh.job) }
    w.refresh.state = "done"
    w.refresh.job = ""
}

/// Updates a place whose last read failed, once the person asks again.
action retry_place(w: Locations, place: Place) (
    icon: .refresh,
    label: "Try again",
    did: "asked for a forecast again",
) {
    place.trouble = ""
    refresh(w, place: .some(place))
}

/// Takes what the device has read so far: each place's answer as it lands, and what the service
/// says of it. What an update reads is nobody's step of their own, so landing it says nothing and
/// offers no undo of its own. A second read of the same place replaces the first.
// ---------------------------------------------------------------- what has landed
action poll_weather(w: Locations) uses time {
    let day = now.day().iso()
    if w.today != day {
        w.today = day
        w.tomorrow = (now + 1d).day().iso()
    }
    if w.refresh.job != "" {
        match engine.status(w.refresh.job) {
            .err(why) => {
                w.refresh.state = "done"
                w.refresh.job = ""
                w.refresh.note = std.say("The update stopped: {why}")
            }
            .ok(said) => match json.decode<Job>(said) {
                .err(why) => {
                    w.refresh.note = std.say("The update's answer did not read: {why}")
                }
                .ok(job) => {
                    w.refresh.done = job.done
                    w.refresh.total = job.total
                    let asked = TakeAsk(job: w.refresh.job)
                    match engine.call("take", json.encode(asked)) {
                        .err(why) => {
                            w.refresh.note = std.say("The update's answer did not read: {why}")
                        }
                        .ok(text) => match json.decode<Taken>(text) {
                            .err(why) => {
                                w.refresh.note = std.say("The update's answer did not read: {why}")
                            }
                            .ok(taken) => {
                                for said in taken.answers {
                                    match json.decode<Answer>(said) {
                                        .err(why) => {
                                            w.refresh.note = std.say(
                                                "A place's answer did not read: {why}",
                                            )
                                        }
                                        .ok(answer) => {
                                            if let place = w.places.find(
                                                    p -> p.id() == answer.place_id,
                                                ) {
                                                if answer.ok {
                                                    place.current.at = answer.time
                                                    place.current.temperature = answer.temperature
                                                    place.current.apparent = answer.apparent
                                                    place.current.humidity = answer.humidity
                                                    place.current.precipitation = answer
                                                        .precipitation
                                                    place.current.code = answer.code
                                                    place.current.wind = answer.wind
                                                    place.current.wind_from = answer.wind_from
                                                    place.current.pressure = answer.pressure
                                                    place.current.uv = answer.uv
                                                    place.current.day = answer.is_day
                                                    place.current.icon = glyph(
                                                        answer.code,
                                                        answer.is_day,
                                                    )
                                                    place.trouble = ""
                                                    place.read = true
                                                    for old in place.hours.map(hour -> hour) {
                                                        place.hours.remove(old)
                                                    }
                                                    for i in answer.hour_at.map((at, i) -> i) {
                                                        place.hours.insert(
                                                            Hour(
                                                                at: answer.hour_at.at(i) ?? "",
                                                                temperature: answer.hour_temp.at(
                                                                    i,
                                                                )
                                                                    ?? 0.0,
                                                                chance: answer.hour_chance.at(i)
                                                                    ?? 0.0,
                                                                code: answer.hour_code.at(i) ?? 0,
                                                                day: answer.hour_day.at(i) ?? true,
                                                                icon: glyph(
                                                                    answer.hour_code.at(i) ?? 0,
                                                                    answer.hour_day.at(i) ?? true,
                                                                ),
                                                            ),
                                                        )
                                                    }
                                                    for old in place.days.map(day -> day) {
                                                        place.days.remove(old)
                                                    }
                                                    for i in answer.day_date.map((date, i) -> i) {
                                                        place.days.insert(
                                                            Day(
                                                                date: answer.day_date.at(i) ?? "",
                                                                weekday: answer.day_weekday.at(i)
                                                                    ?? 0,
                                                                high: answer.day_high.at(i) ?? 0.0,
                                                                low: answer.day_low.at(i) ?? 0.0,
                                                                chance: answer.day_chance.at(i)
                                                                    ?? 0.0,
                                                                code: answer.day_code.at(i) ?? 0,
                                                                wind: answer.day_wind.at(i) ?? 0.0,
                                                                uv: answer.day_uv.at(i) ?? 0.0,
                                                                rise: answer.day_rise.at(i) ?? "",
                                                                set: answer.day_set.at(i) ?? "",
                                                                icon: glyph(
                                                                    answer.day_code.at(i) ?? 0,
                                                                    true,
                                                                ),
                                                            ),
                                                        )
                                                    }
                                                    place.has_air = answer.air_ok
                                                    if answer.air_ok {
                                                        place.air.at = answer.air_at
                                                        place.air.european = answer.air_european
                                                        place.air.us = answer.air_us
                                                        place.air.pm2_5 = answer.air_pm2_5
                                                        place.air.pm10 = answer.air_pm10
                                                        place.air.no2 = answer.air_no2
                                                        place.air.ozone = answer.air_ozone
                                                    }
                                                    place.fetched = at(answer.fetched)
                                                } else {
                                                    place.trouble = why_answer(answer)
                                                    w.refresh.failed += 1
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                    }
                    if job.state == "done" or job.state == "cancelled" {
                        w.refresh.state = "done"
                        w.refresh.job = ""
                    }
                }
            },
        }
    } else if w.refresh.note == "" and w.places.any(p -> not p.read and p.trouble == "") {
        // A place nothing has been read of yet is read the first time the app runs here, unless
        // its last read failed: a service that refused stays refused until a person asks again.
        refresh(w)
    }
    // What a person is looking for: the device is asked once per query, so typing starts one read
    // rather than one per keystroke, and the last of them is the one that stands.
    if let reader = w.searches.first() {
        if reader.wanted.trim() != reader.asked {
            if reader.job != "" {
                engine.cancel(reader.job)
                reader.job = ""
            }
            let query = reader.wanted.trim()
            if query.len() < 2 {
                reader.asked = query
                for old in reader.found.map(place -> place) {
                    reader.found.remove(old)
                }
                reader.searching = false
                reader.note = ""
            } else {
                match engine.start("search", json.encode(SearchAsk(query: query))) {
                    .ok(job) => {
                        reader.job = job
                        reader.asked = query
                        reader.searching = true
                        for old in reader.found.map(place -> place) {
                            reader.found.remove(old)
                        }
                        reader.note = ""
                    }
                    .err(why) => {
                        reader.asked = query
                        reader.searching = false
                        reader.note = std.say("Couldn't look for places: {why}")
                    }
                }
            }
        }
        if reader.job != "" {
            match engine.status(reader.job) {
                .err(why) => {
                    reader.job = ""
                    reader.searching = false
                    reader.note = std.say("The place search stopped: {why}")
                }
                .ok(said) => match json.decode<Job>(said) {
                    .err(_) => {}
                    .ok(job) => {
                        let asked = TakeAsk(job: reader.job)
                        match engine.call("take", json.encode(asked)) {
                            .err(_) => {}
                            .ok(text) => match json.decode<Taken>(text) {
                                .err(_) => {}
                                .ok(taken) => {
                                    if taken.error != "" {
                                        reader.note = taken.error
                                    }
                                    for old in reader.found.map(place -> place) {
                                        reader.found.remove(old)
                                    }
                                    for said in taken.places {
                                        match json.decode<Found>(said) {
                                            .err(_) => {}
                                            .ok(place) => { reader.found.insert(place) }
                                        }
                                    }
                                }
                            },
                        }
                        if job.state == "done" or job.state == "cancelled" {
                            reader.job = ""
                            reader.searching = false
                        }
                    }
                },
            }
        }
    }
    // A reading that has stood for hours says so, so the page never calls it current.
    for place in w.places {
        let stale = if let read = place.fetched { (now - read).seconds() >= 7200 } else { false }
        if place.stale != stale {
            place.stale = stale
        }
    }
}

/// The forecasts are taken as they land, and a place nothing is known of is read once.
trigger watch_weather(w: Locations) every 1s: poll_weather(w)

/// Asks the device for `places`' forecasts and air, answered with the job's id.
fn ask_update(places: List<Place>) -> Result<Text, Text> {
    let asked = Ask(
        places: places.map(
            p -> AskPlace(place_id: p.id(), latitude: p.latitude, longitude: p.longitude),
        ),
    )
    engine.start("refresh", json.encode(asked))
}

/// What an answer says, kept on the place: its reading written field by field, its hours and
/// days made into the place's own records, and its air when the second service answered.
///
/// Why a place could not be read, as its page says it.
fn why_answer(answer: Answer) -> Text {
    if answer.reason == "unreachable" {
        "Couldn't reach the weather service. Check your connection."
    } else if answer.reason == "refused" {
        if answer.status > 0 {
            std.say("The weather service refused the request (HTTP {answer.status})")
        } else {
            "The weather service refused the request"
        }
    } else if answer.reason == "not_a_reading" {
        "The answer wasn't a forecast"
    } else if answer.reason == "unavailable" {
        "This device reaches no weather service"
    } else { answer.message }
}

/// A moment from the epoch seconds an answer carries, when it names one.
fn at(seconds: Int) -> Option<Time> {
    if seconds <= 0 {
        .none
    } else {
        .some(Time("1970-01-01T00:00:00Z") + Duration(seconds))
    }
}

/// Whether `place` stands within a twentieth of a degree of a point: about five kilometres.
fn near(place: Place, latitude: Float, longitude: Float) -> Bool {
    (place.latitude - latitude).abs() < 0.05 and (place.longitude - longitude).abs() < 0.05
}

/// Whether a place's reading is missing or old enough to fetch again: the service keeps to its
/// hour, and a reading of two hours is no longer the current one.
fn needs_update(place: Place, moment: Time) -> Bool {
    if place.trouble != "" {
        false
    } else if let read = place.fetched { (moment - read).seconds() >= 7200 } else { true }
}

/// Opening the window reads the places whose forecast is missing or no longer current. Nothing
/// is read while it stands, so opening a window again costs nothing.
opened action arrive(w: Locations) uses time {
    if w.refresh.job == "" and w.places.any(p -> needs_update(p, now)) { refresh(w) }
}

/// Looks for places by name. The device asks the service away from the frame; the window takes
/// the rows as they land.
// ---------------------------------------------------------------- looking for a place
action begin_search(w: Locations) -> Searching (icon: .search, label: "Look for a place") {
    let found = w.searches.first() ?? w.searches.insert(Searching())
    if found.job != "" { engine.cancel(found.job) }
    found.wanted = ""
    found.asked = ""
    found.job = ""
    found.searching = false
    for old in found.found.map(place -> place) {
        found.found.remove(old)
    }
    found.note = ""
    found
}

/// Empties the field and stops looking.
action end_search(w: Locations, reader: Searching) {
    if reader.job != "" { engine.cancel(reader.job) }
    reader.wanted = ""
    reader.asked = ""
    reader.job = ""
    reader.searching = false
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
    if reader.searching {
        "Looking for places…"
    } else if reader.wanted.trim().len() < 2 {
        "Type two letters or more"
    } else if reader.note != "" {
        reader.note
    } else if reader.found.is_empty() { "No places found" } else { "" }
}

/// The tile of one hour: its clock, its sky, its temperature and how likely rain is.
// ---------------------------------------------------------------- the window
view hour_tile(hour: Hour, imperial: Bool, now_at: Text) {
    stack(gap: .tight, align: .center) {
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
view hours_strip(place: Place, imperial: Bool, now_at: Text) {
    if place.hours.is_empty() {
        ui.Text("No hour-by-hour forecast yet.", role: .caption, secondary: true)
    } else {
        scroll(axis: .horizontal) {
            row(gap: .related) {
                for hour in place.hours {
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
    row(gap: .related, align: .center) {
        ui.Text(day_name(day, today, tomorrow), role: .body, min: 5rem)
        ui.Icon(day.icon, description: condition(day.code))
        ui.Text(condition(day.code), role: .body, secondary: true, truncate)
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
view hero(place: Place, imperial: Bool) {
    let reading = place.current
    row(gap: .group, align: .center) {
        ui.Icon(reading.icon, size: .large, description: condition(reading.code))
        stack(gap: .tight) {
            ui.Text(degree_words(reading.temperature, imperial), role: .display, numeric: true)
            ui.Text(condition(reading.code), role: .heading)
            if let today = place.days.first() {
                ui.Text(
                    "High {degrees(today.high, imperial)}° · low {degrees(today.low, imperial)}°",
                    role: .body,
                    secondary: true,
                )
            }
            ui.Text(subtitle(place), role: .caption, secondary: true)
            row(gap: .tight, align: .center) {
                if let fetched = place.fetched {
                    ui.Time(fetched, role: .caption, secondary: true)
                }
                if place.stale {
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
        ui.StatTile("Feels like", "{degrees(reading.apparent, imperial)}", unit: "°")
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
    let job = w.refresh
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

/// The shown place's own commands: update it, show it first, move it to the top, or remove it.
view place_bar(w: Locations, place: Place) {
    row(gap: .tight, wrap: true) {
        ui.Button("Refresh", icon: .refresh) {
            on press: refresh(w, place: .some(place))
        }
        ui.Button("Show first", icon: .pin) {
            on press: use_as_default(w, place)
        }
        ui.Button("Move to top", icon: .arrow_up) {
            on press: move_to_top(w, place)
        }
        ui.Button("Remove place", icon: .trash_2, role: .destructive) {
            on press: remove_place(w, place)
        }
    }
}

/// One place's forecast: its hero, its readings, the hours, the week and the air, or what stands
/// in their place while the device reads it.
view forecast(w: Locations, place: Place, imperial: Bool) {
    page {
        stack(gap: .group) {
            update_bar(w)
            place_bar(w, place)
            if place.trouble != "" {
                ui.Banner(
                    "Couldn't update this forecast",
                    message: place.trouble,
                    kind: .warning,
                    icon: .cloud_off,
                    action: "Try again",
                ) {
                    on act: retry_place(w, place)
                }
            }
            if place.read {
                hero(place, imperial)
                readings(place.current, imperial)
                if let today = place.days.first() {
                    if let fraction = daylight_fraction(place.current.at, today) {
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
                    hours_strip(place, imperial, place.current.at)
                }
                ui.Section("The week ahead") {
                    let lows = place.days.map(d -> d.low).sort_by(x -> x)
                    let highs = place.days.map(d -> d.high).sort_by(x -> x)
                    let low = lows.first() ?? 0.0
                    let high = highs.last() ?? 1.0
                    stack(gap: .related) {
                        for day in place.days {
                            day_row(day, low, high, imperial, w.today, w.tomorrow)
                        }
                    }
                }
                if place.has_air {
                    ui.Section("Air quality") {
                        air_quality(place.air)
                    }
                }
            } else if place.trouble == "" {
                stack(gap: .group) {
                    ui.Skeleton(shape: .page)
                    ui.Text("Reading the forecast…", role: .caption, secondary: true)
                }
            }
        }
    }
}

/// The places a person follows, in their order, each with its sky, and the choice the list
/// reports.
view places_side(w: Locations, shown: Option<Place>, chosen: State<Option<Place>>) {
    ui.NavList(selected: shown_key(w, shown), title: "Places") {
        for place in w.places {
            if place.read {
                ui.NavItem(place.id(), place.name, icon: place.current.icon, item: place) {
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
    let imperial = settings.locale.measurement == .Imperial
    let place = shown_place(w, chosen)
    ui.NavShell(content_kind: .page, sidebar_title: "Weather", title: place?.name ?? "Weather") {
        actions: {
            row(gap: .tight) {
                ui.Button("Add place", icon: .plus) {
                    on press: set(adding, true).then(begin_search(w))
                }
                if place != .none {
                    ui.Button("Refresh", icon: .refresh) {
                        on press: refresh(w)
                    }
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
                forecast(w, shown, imperial)
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
    amsterdam.current.at = "2026-11-24T09:00"
    amsterdam.current.temperature = 7.4
    amsterdam.current.apparent = 4.8
    amsterdam.current.humidity = 82.0
    amsterdam.current.precipitation = 0.4
    amsterdam.current.code = 61
    amsterdam.current.wind = 18.0
    amsterdam.current.wind_from = 210.0
    amsterdam.current.pressure = 1012.0
    amsterdam.current.uv = 1.0
    amsterdam.current.day = true
    amsterdam.current.icon = "cloud_rain"
    amsterdam.read = true
    amsterdam.fetched = .some(2026-11-24T07:20Z)
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
        amsterdam.hours.insert(hour)
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
        amsterdam.days.insert(day)
    }
    amsterdam.air.at = "2026-11-24T09:00"
    amsterdam.air.european = 32.0
    amsterdam.air.us = 41.0
    amsterdam.air.pm2_5 = 8.4
    amsterdam.air.pm10 = 14.2
    amsterdam.air.no2 = 18.0
    amsterdam.has_air = true
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
    reykjavik.current.at = "2026-11-24T08:00"
    reykjavik.current.temperature = -1.6
    reykjavik.current.apparent = -7.0
    reykjavik.current.humidity = 74.0
    reykjavik.current.code = 71
    reykjavik.current.wind = 42.0
    reykjavik.current.wind_from = 20.0
    reykjavik.current.pressure = 998.0
    reykjavik.current.day = false
    reykjavik.current.icon = "moon"
    reykjavik.read = true
    reykjavik.fetched = .some(2026-11-24T07:20Z)
    reykjavik.days.insert(
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
