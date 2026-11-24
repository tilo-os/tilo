app so.tilo.speed-test "Speed Test" (
    icon: .gauge,
    category: .system,
    tagline: "Measures download, upload and latency",
    keywords: ["speedtest", "internet speed", "bandwidth", "ping", "latency", "download speed", "upload", "network test", "connection speed", "mbps"],
    web: ["speed.cloudflare.com"],
)

use so.tilo.device as device

/// The connections this workspace measured: one entry for every finished run, newest first, and
/// the run the window shows, while it measures and after it ends.
type SpeedTest {
    /// Every finished run, newest first.
    results: Collection<Measurement>
    /// The run on the page: the one measuring now, and the one that just ended.
    runs: Collection<Run>
    /// When the device is asked how far the run has got; left behind when the run ends, so
    /// nothing asks again.
    next_poll: Option<Time> = .none
}

root speed_test: SpeedTest

default view speed_test for SpeedTest

/// One finished run: what it measured, where, and on which device.
type Measurement {
    /// Round trip to the server, in milliseconds.
    ping: Float
    /// Variation of the round trip, in milliseconds.
    jitter: Float
    /// Download speed, in megabits per second.
    download: Float
    /// Upload speed, in megabits per second.
    upload: Float
    /// The server that answered.
    server: Text
    /// The device that ran it.
    device: Text
}

/// A run of the test: where it stands, what it has read so far, and why it stopped early.
type Run {
    /// Where the run stands: `ping`, `download`, `upload`, `complete` or `error`.
    phase: Text = "ping"
    /// The reading of the stage in progress: milliseconds while pinging, megabits per second
    /// after.
    live: Float = 0.0
    /// The bytes the run moved so far.
    bytes: Int merge counter = 0
    /// The server that answered.
    server: Text = ""
    /// What the ping stage measured, for the run's record.
    ping: Float = 0.0
    jitter: Float = 0.0
    /// What the download stage measured.
    download: Float = 0.0
    /// Why the run stopped early, in the words of the stage that failed.
    error: Text = ""
}

/// The most runs the workspace keeps: the newest ones, and the oldest go first.
fn kept() -> Int {
    20
}

/// The server every run measures against: Cloudflare's test endpoint.
fn server_address() -> Text {
    "https://speed.cloudflare.com"
}

/// The bytes one reply carries: the language holds a reply whole, so a run asks for its
/// download in chunks of this size.
fn chunk_bytes() -> Int {
    2000000
}

/// The bytes one run uploads.
fn upload_bytes() -> Int {
    2000000
}

/// Megabits per second, from a byte count and how long it took.
fn mbps(bytes: Int, milliseconds: Float) -> Float {
    if milliseconds <= 0.0 {
        0.0
    } else {
        bytes.to_float() * 8.0 / (milliseconds / 1000.0) / 1000000.0
    }
}

/// The median of a run's samples: what a round trip usually takes.
fn median(samples: List<Float>) -> Float {
    let sorted = samples.sort_by(x -> x)
    let count = sorted.len()
    if count == 0 {
        0.0
    } else {
        let mid = count / 2
        if count % 2 == 0 {
            ((sorted.at(mid - 1) ?? 0.0) + (sorted.at(mid) ?? 0.0)) / 2.0
        } else { sorted.at(mid) ?? 0.0 }
    }
}

/// How much the round trip varies: the mean step between samples, in the order they came.
fn jitter(samples: List<Float>) -> Float {
    let count = samples.len()
    if count < 2 {
        0.0
    } else {
        let total = 0.0
        for i in samples.map((x, i) -> i) {
            if i > 0 {
                total = total + ((samples.at(i) ?? 0.0) - (samples.at(i - 1) ?? 0.0)).abs()
            }
        }
        total / (count - 1).to_float()
    }
}

/// The server that answered, by the Cloudflare location its answer names.
fn colo_of(answer: Response) -> Text {
    let line = answer.headers.find(h -> h.lower().starts_with("cf-meta-colo:")) ?? ""
    if line == "" {
        "Cloudflare"
    } else {
        let colo = (line.split(":").last() ?? "").trim()
        if colo == "" { "Cloudflare" } else { "Cloudflare at {colo}" }
    }
}

/// Why a stage could not measure, in the words the banner shows.
fn trouble_of(answer: Response, stage: Text) -> Text {
    if answer.status == 0 {
        std.say("{stage} failed: {answer.status_text}")
    } else {
        std.say("{stage} failed: the server refused the request (HTTP {answer.status})")
    }
}

/// Starts a run: the window measures latency against the nearest Cloudflare test server, then
/// the download and the upload on the seconds after, and shows the dial moving as it goes.
primary action start(st: SpeedTest) uses time, web (
    icon: .play,
    label: "Run speed test",
    did: "ran a speed test",
) {
    applies st.runs.is_empty() "A test is running"
    let run = st.runs.insert(Run())
    st.next_poll = .none
    if ping_stage(run) {
        run.phase = "download"
        run.live = 0.0
        st.next_poll = .some(now + 1s)
    }
}

/// Stops the run in flight: the measurement stops and the dial goes back to ready. Nothing of a
/// stopped run is kept.
action stop(st: SpeedTest) uses time (
    icon: .square,
    label: "Stop the test",
    did: "stopped a speed test",
) {
    applies not st.runs.is_empty() "No test is running"
    st.next_poll = .none
    for run in st.runs {
        st.runs.remove(run)
    }
}

/// Measures the next stage of the run on the page: the download, then the upload, which records
/// what the run measured. A stage that fails ends the run with its words.
action poll(st: SpeedTest) uses time, web (label: "Ask how far the test has got") {
    let current = st.runs.first()
    if let run = current {
        st.next_poll = .none
        if run.phase == "download" {
            if download_stage(run) {
                run.phase = "upload"
                st.next_poll = .some(now + 1s)
            }
        } else if run.phase == "upload" { upload_stage(st, run) }
    }
}

/// Times a few round trips and keeps the server the first one found. The dial shows the last
/// reading while the run measures.
action ping_stage(run: Run) -> Bool uses web {
    let server = server_address()
    let samples: List<Float> = []
    for i in [1, 2, 3, 4, 5, 6] {
        let answer = web.get("{server}/__down?bytes=1")
        if not answer.ok {
            run.phase = "error"
            run.error = trouble_of(answer, "Ping")
            return false
        }
        if i == 1 {
            run.server = colo_of(answer)
        } else {
            let took = answer.duration_ms.to_float()
            samples = samples + [took]
            run.live = took
        }
    }
    let measured = median(samples)
    run.ping = measured
    run.jitter = jitter(samples)
    run.live = measured
    true
}

/// Downloads a run's bytes in chunks of a reply the language holds whole, timing them together,
/// and keeps the rate they came at.
action download_stage(run: Run) -> Bool uses web {
    let server = server_address()
    let bytes = 0
    let took = 0.0
    for i in [1, 2, 3, 4, 5] {
        let answer = web.get("{server}/__down?bytes={chunk_bytes()}")
        if not answer.ok {
            run.phase = "error"
            run.error = trouble_of(answer, "Download test")
            return false
        }
        let chunk = answer.body.len()
        bytes = bytes + chunk
        run.bytes += chunk
        took = took + answer.duration_ms.to_float()
        run.live = mbps(bytes, took)
    }
    run.download = mbps(bytes, took)
    run.live = run.download
    true
}

/// Uploads a run's bytes and records what the whole run measured. A refusal fails the stage.
action upload_stage(st: SpeedTest, run: Run) uses web {
    let server = server_address()
    let answer = web.post("{server}/__up", bytes: upload_bytes())
    if not answer.ok {
        run.phase = "error"
        run.error = trouble_of(answer, "Upload test")
        return
    }
    let measured = mbps(upload_bytes(), answer.duration_ms.to_float())
    run.live = measured
    run.bytes += upload_bytes()
    run.phase = "complete"
    record(st, run.ping, run.jitter, run.download, measured, run.server, device.host_info.hostname)
}

/// Records a finished measurement, newest first, and lets the oldest go beyond what the
/// workspace keeps: an agent or a script may record one too.
action record(
    st: SpeedTest,
    ping: Float,
    jitter: Float,
    download: Float,
    upload: Float,
    server: Text,
    device: Text,
) (label: "Record a measurement", did: "recorded a measurement") {
    let made = st.results.insert(
        Measurement(
            ping: ping,
            jitter: jitter,
            download: download,
            upload: upload,
            server: server,
            device: device,
        ),
    )
    st.results.move(made, to: 0)
    if st.results.len() > kept() {
        if let oldest = st.results.last() { st.results.remove(oldest) }
    }
}

/// Forgets every measurement.
destructive action clear(st: SpeedTest) (
    icon: .trash,
    label: "Clear results",
    did: "cleared the measurements",
) {
    applies not st.results.is_empty() "No test has been measured"
    for m in st.results {
        st.results.remove(m)
    }
}

/// Asks the device how far the run has got, a second after it last said: the workspace moves its
/// own time forward while the run measures, and stops doing so when it ends.
trigger poll_run(st: SpeedTest) at st.next_poll: poll(st)

/// The whole window: the dial and its one action beside what the run measured, which stacks
/// below it in a narrow window. Before anything is measured, the test stands alone in the middle.
// ---------------------------------------------------------------- the window
view speed_test(st: SpeedTest) {
    let run = st.runs.first()
    let phase = if let r = run { r.phase } else { "idle" }
    if phase == "idle" and st.results.is_empty() {
        ready_page(st, run)
    } else {
        std.page(st, canvas: true, fill: true) {
            if let r = run {
                if r.phase == "error" {
                    ui.Banner("Test interrupted", message: r.error, kind: .error)
                }
            }
            split(side: .trailing, collapse: .stack) {
                primary(min: 20rem) {
                    test_column(st, run, phase)
                }
                secondary(min: 18rem) {
                    results_column(st, run, phase)
                }
            }
        }
    }
}

/// The page before any test: the dial, the action and the server it measures against, centred in
/// the window rather than a dial in a corner beside an empty column.
view ready_page(st: SpeedTest, run: Option<Run>) {
    let server = server_of(st, run)
    std.page(st, canvas: true, fill: true) {
        stack(fill) {
            spacer
            stack(gap: .section, align: .center) {
                dial(st, run, "idle")
                text("Ready to test", role: .subheading)
                button(start(st))
                text(
                    if server == "" or server == "Cloudflare" {
                        "Measures against the nearest Cloudflare server"
                    } else { "Measures against {server}" },
                    role: .caption,
                    secondary: true,
                )
            }
            spacer
        }
    }
}

/// The dial, what it is doing, the one action, and the three stages the device measures in.
view test_column(st: SpeedTest, run: Option<Run>, phase: Text) {
    stack(gap: .section, align: .center) {
        dial(st, run, phase)
        text(
            if phase == "ping" {
                "Measuring latency"
            } else if phase == "download" {
                "Measuring download"
            } else if phase == "upload" {
                "Measuring upload"
            } else if phase == "complete" {
                "Test complete"
            } else if phase == "error" { "Ready to try again" } else { "Ready to test" },
            role: .subheading,
        )
        if running(phase) {
            button(stop(st))
        } else {
            button(start(st))
        }
        if phase != "idle" and phase != "error" {
            ui.Stepper(steps: ["Ping", "Download", "Upload"], current: .some(stage_of(phase)))
        }
    }
}

/// The measured numbers, the facts about the test, what the connection is good for, and the runs
/// before this one.
view results_column(st: SpeedTest, run: Option<Run>, phase: Text) {
    let newest = st.results.first()
    stack(gap: .group) {
        if let n = newest {
            if phase == "complete" {
                section("Results") {
                    ui.StatGroup {
                        ui.StatTile("Ping", whole(n.ping), unit: "ms")
                        ui.StatTile("Download", one(n.download), unit: "Mbps")
                        ui.StatTile("Upload", one(n.upload), unit: "Mbps")
                    }
                }
            }
        }
        section("Connection") {
            ui.PropertyList {
                ui.Property("Server", server_of(st, run))
                if let n = newest {
                    ui.Property("Jitter", "{one(n.jitter)} ms")
                }
                if let r = run {
                    if r.bytes > 0 {
                        ui.Property("Data used", size_of(r.bytes))
                    }
                }
            }
        }
        if let n = newest {
            if phase == "complete" {
                section("Connection quality") {
                    stack(gap: .tight) {
                        quality_row(
                            "Video calls",
                            n.upload >= 8.0 and n.ping <= 80.0,
                            n.upload >= 3.0,
                        )
                        quality_row("4K streaming", n.download >= 50.0, n.download >= 25.0)
                        quality_row(
                            "Cloud gaming",
                            n.ping <= 20.0 and n.jitter <= 5.0,
                            n.ping <= 50.0,
                        )
                    }
                }
            }
        }
        if not st.results.is_empty() {
            section("Recent tests") {
                for m in st.results {
                    if phase != "complete" or newest != .some(m) {
                        recent_row(m)
                    }
                }
            }
        }
    }
}

/// One kind of use and how well the connection carries it, as a status the toolkit colours.
view quality_row(use_case: Text, excellent: Bool, good: Bool) {
    row(distribute: .between, align: .center) {
        text(use_case)
        if excellent or good {
            ui.Badge("Excellent", tone: .success)
        } else {
            ui.Badge("Limited", tone: .warning)
        }
    }
}

/// A run before this one: what it measured, how fast it went each way, and when it ran.
view recent_row(m: Measurement) {
    ui.ListRow(
        title: "{one(m.download)} Mbps down",
        secondary: "{one(m.upload)} Mbps up, {whole(m.ping)} ms ping",
        item: m,
    ) {
        trailing: {
            if let made = created(m) {
                ui.Time(made.at, secondary: true)
            }
        }
    }
}

/// The dial: one hero reading, its arc growing with the speed measured, and the unit under it.
view dial(st: SpeedTest, run: Option<Run>, phase: Text) {
    ui.ProgressRing(
        arc_of(st, run, phase),
        readout: readout_of(st, run, phase),
        caption: unit_of(phase),
        size: .large,
    )
}

/// The server that answered, or the one a run in flight found.
fn server_of(st: SpeedTest, run: Option<Run>) -> Text {
    if let r = run {
        if r.server != "" {
            return r.server
        }
    }
    if let n = st.results.first() {
        return n.server
    }
    ""
}

/// Whether the run is measuring now.
fn running(phase: Text) -> Bool {
    phase == "ping" or phase == "download" or phase == "upload"
}

/// Which of the three stages the stepper shows, and 3 once the test is complete.
fn stage_of(phase: Text) -> Int {
    if phase == "download" {
        1
    } else if phase == "upload" { 2 } else if phase == "complete" { 3 } else { 0 }
}

/// The reading of the stage in progress.
fn live_of(run: Option<Run>) -> Float {
    if let r = run { r.live } else { 0.0 }
}

/// What the finished run measured down, while the dial shows a complete run.
fn download_of(st: SpeedTest) -> Float {
    if let n = st.results.first() { n.download } else { 0.0 }
}

/// How far the dial's arc runs: bounded, and rising with the speed.
fn arc_of(st: SpeedTest, run: Option<Run>, phase: Text) -> Float {
    let live = live_of(run)
    if phase == "ping" {
        (live / 50.0).clamp(0.08, 0.92)
    } else if phase == "download" {
        (live / 750.0).clamp(0.06, 1.0)
    } else if phase == "upload" {
        (live / 180.0).clamp(0.06, 1.0)
    } else if phase == "complete" {
        (download_of(st) / 800.0).clamp(0.08, 1.0)
    } else { 0.0 }
}

/// The number in the dial's middle: nothing at all before a run, never a dash for a number.
fn readout_of(st: SpeedTest, run: Option<Run>, phase: Text) -> Text {
    if phase == "ping" {
        whole(live_of(run))
    } else if phase == "download" or phase == "upload" {
        one(live_of(run))
    } else if phase == "complete" { one(download_of(st)) } else { "" }
}

/// What the number in the dial measures.
fn unit_of(phase: Text) -> Text {
    if phase == "ping" {
        "ms"
    } else if phase == "download" or phase == "upload" or phase == "complete" { "Mbps" } else { "" }
}

/// A number of a few hundred as a person reads it: one decimal.
fn one(v: Float) -> Text {
    let text = v.round(1).to_text()
    if text.contains(".") { text } else { "{text}.0" }
}

/// A whole number, as milliseconds and pings read.
fn whole(v: Float) -> Text {
    v.round().to_text()
}

/// How many bytes read to a person: `12 MB`.
fn size_of(n: Int) -> Text {
    if n < 1000 {
        "{n} B"
    } else if n < 1000000 {
        "{n / 1000} KB"
    } else if n < 1000000000 { "{n / 1000000} MB" } else { "{n / 1000000000} GB" }
}

/// Three runs of one connection, the newest measure on the dial.
// ---------------------------------------------------------------- what people meet
scenario "three measurements of one connection" {
    record(speed_test, 14.1, 2.2, 584.6, 109.3, "Cloudflare at LHR", "Laptop")
    record(speed_test, 12.4, 1.8, 618.2, 112.9, "Cloudflare at LHR", "Laptop")
    record(speed_test, 11.8, 1.6, 642.7, 118.4, "Cloudflare at LHR", "Laptop")
    speed_test.runs.insert(
        Run(phase: "complete", live: 118.4, bytes: 12000000, server: "Cloudflare at LHR"),
    )
}

/// A test measuring its download, as a person watches the dial.
scenario "a test measuring its download" {
    speed_test.runs.insert(Run(phase: "download", live: 386.4, server: "Cloudflare at LHR"))
    speed_test.next_poll = .none
}

/// A slow connection: what makes even the newest podcast stutter.
scenario "a slow connection" {
    record(speed_test, 120.0, 20.0, 9.4, 1.2, "Cloudflare at FRA", "Old laptop")
    speed_test.runs.insert(
        Run(phase: "complete", live: 1.2, bytes: 12000000, server: "Cloudflare at FRA"),
    )
}

/// A test that stopped: the server would not answer.
scenario "a test that could not reach the server" {
    speed_test.runs.insert(
        Run(phase: "error", error: "Could not reach the test server: connection refused"),
    )
}
