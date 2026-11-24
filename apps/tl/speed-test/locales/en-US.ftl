# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-speed-test = Speed Test

d-so-tilo-speed-test-keywords = speedtest, internet speed, bandwidth, ping, latency, download speed, upload, network test, connection speed, mbps

d-kobrqlxh = Speed test
    .doc = The connections this workspace measured: one entry for every finished run, newest first, and the run the window shows, while it measures and after it ends.
    .noun = { $count ->
        [one] speed test
       *[other] speed tests
    }

d-hpwi5yye = Results
    .doc = Every finished run, newest first.

d-phw7nc7e = Runs
    .doc = The run on the page: the one measuring now, and the one that just ended.

d-ckq5elpb = Next poll
    .doc = When the device is asked how far the run has got; left behind when the run ends, so nothing asks again.

d-kobrqlxh-filter-next_poll = Next poll

d-l5v53ewu = Measurement
    .doc = One finished run: what it measured, where, and on which device.
    .noun = { $count ->
        [one] measurement
       *[other] measurements
    }

d-5f54c4ou = Ping
    .doc = Round trip to the server, in milliseconds.

d-q5jjysbc = Jitter
    .doc = Variation of the round trip, in milliseconds.

d-kded6ppq = Download
    .doc = Download speed, in megabits per second.

d-x6xgknqc = Upload
    .doc = Upload speed, in megabits per second.

d-vcibj65j = Server
    .doc = The server that answered.

d-vbch5rhv = Device
    .doc = The device that ran it.

d-l5v53ewu-filter-ping = Ping

d-l5v53ewu-filter-jitter = Jitter

d-l5v53ewu-filter-download = Download

d-l5v53ewu-filter-upload = Upload

d-ylzbkpat = Run
    .doc = A run of the test: where it stands, what it has read so far, and why it stopped early.
    .noun = { $count ->
        [one] run
       *[other] runs
    }

d-fbbyyume = Phase
    .doc = Where the run stands: `ping`, `download`, `upload`, `complete` or `error`.

d-376ly3mp = Job
    .doc = The device job measuring, while one runs; empty between runs.

d-vufvfoij = Live
    .doc = The reading of the stage in progress: milliseconds while pinging, megabits per second after.

d-stw2oyj6 = Bytes
    .doc = The bytes the run moved so far.

d-nwtvekek = Server
    .doc = The server that answered.

d-wkhwlknc = Error
    .doc = Why the run stopped early, in the words of the stage that failed.

d-ylzbkpat-filter-live = Live

d-ylzbkpat-filter-bytes = Bytes

d-bofeq5tj = Run state
    .doc = What the device answers about a run (`engine.status`).
    .noun = { $count ->
        [one] run state
       *[other] run states
    }

d-4xzztkvp = State

d-os4ndogp = Phase

d-7eyrzvhi = Live

d-phil2ezm = Bytes

d-5szkqkfk = Server

d-hcm22tea = Ping

d-esi4hzhw = Jitter

d-gmjsrtmo = Download

d-q4qvdsia = Upload

d-eyu5ljwd = Device

d-c6eopm2r = Failure

d-bofeq5tj-filter-live = Live

d-bofeq5tj-filter-bytes = Bytes

d-bofeq5tj-filter-ping = Ping

d-bofeq5tj-filter-jitter = Jitter

d-bofeq5tj-filter-download = Download

d-bofeq5tj-filter-upload = Upload

d-divjfr4m = Started
    .doc = What the device answers when a run starts (`engine.start`).
    .noun = { $count ->
        [one] started
       *[other] starteds
    }

d-fltipwm6 = Job

d-gs3ocynq = Run speed test
    .doc = Starts a run: the device measures latency, download and upload against the nearest Cloudflare test server, and the window follows it as it goes.
    .did = ran a speed test
    .w-a-test-is-running = A test is running

d-eoox4ss7 = Stop the test
    .doc = Stops the run in flight: the device stops measuring and the dial goes back to ready.
    .did = stopped a speed test
    .w-no-test-is-running = No test is running

d-crsodmy5 = Ask how far the test has got
    .doc = Asks the device how far the run has got: the dial follows the reading of the stage, and what the run measured is kept when it ends.

d-zpm53plh = Record a measurement
    .doc = Records a finished measurement, newest first, and lets the oldest go beyond what the workspace keeps: an agent or a script may record one too.
    .did = recorded a measurement

d-zpm53plh-ping = Ping

d-zpm53plh-jitter = Jitter

d-zpm53plh-download = Download

d-zpm53plh-upload = Upload

d-zpm53plh-server = Server

d-zpm53plh-device = Device

d-7joaum2r = Clear results
    .doc = Forgets every measurement.
    .did = cleared the measurements
    .w-no-test-has-been-measured = No test has been measured

d-apn5jyhr = Speed test
    .w-idle = idle
    .w-test-interrupted = Test interrupted

d-waf6xz4h = Ready page
    .w-idle = idle
    .w-ready-to-test = Ready to test
    .w-measures-against-the-nearest-cloudflare-server = Measures against the nearest Cloudflare server
    .w-measures-against = Measures against { $server }

d-x2kfhyxc = Test column
    .w-measuring-latency = Measuring latency
    .w-measuring-download = Measuring download
    .w-measuring-upload = Measuring upload
    .w-test-complete = Test complete
    .w-ready-to-try-again = Ready to try again
    .w-ready-to-test = Ready to test
    .w-ping = Ping
    .w-download = Download
    .w-upload = Upload

d-qy655f3m = Results column
    .w-results = Results
    .w-ping = Ping
    .w-ms = ms
    .w-download = Download
    .w-mbps = Mbps
    .w-upload = Upload
    .w-connection = Connection
    .w-server = Server
    .w-jitter = Jitter
    .w-ms-2 = { $one } ms
    .w-data-used = Data used
    .w-connection-quality = Connection quality
    .w-video-calls = Video calls
    .w-4k-streaming = 4K streaming
    .w-cloud-gaming = Cloud gaming
    .w-recent-tests = Recent tests

d-ergxoae7 = Quality row
    .w-excellent = Excellent
    .w-limited = Limited

d-l4t3t7cr = Recent row
    .w-mbps-down = { $one } Mbps down
    .w-mbps-up-ms-ping = { $one } Mbps up, { $whole } ms ping

d-jsy7gexo = Dial
