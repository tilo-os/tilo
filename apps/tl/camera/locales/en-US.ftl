# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-camera = Camera

d-so-tilo-camera-keywords = camera, photo, photos, picture, video, record, code, qr, barcode, scan, scanner, document, webcam, kamera, foto, fotos, bild, video, aufnehmen, code, qr, barcode, scannen, scanner, dokument, webcam

d-sxhgr2t2 = Scan entry
    .doc = One code the person scanned, what it said and when it was seen.
    .noun = { $count ->
        [one] scan entry
       *[other] scan entries
    }

d-hsfv5zso = Raw

d-ep2nakxy = Ts

d-sxhgr2t2-filter-ts = Ts

d-7t7msvmc = Shared capture
    .doc = The capture taken last, handed to the app that owns its bytes: the picture or clip itself, what it is, and the lens and size it was taken with.
    .noun = { $count ->
        [one] shared capture
       *[other] shared captures
    }

d-3w536lga = File

d-2h53klqs = Kind

d-7enc2ly3 = Ts

d-dwoxfhf4 = W

d-wwonv6b5 = H

d-cchj7ycb = Dur ms

d-ls5awfet = Facing

d-4xq6x7ig = Label

d-7t7msvmc-filter-kind = Kind

d-7t7msvmc-filter-ts = Ts

d-7t7msvmc-filter-w = W

d-7t7msvmc-filter-h = H

d-7t7msvmc-filter-dur_ms = Dur ms

d-7t7msvmc-filter-facing = Facing

d-4gga5k7s = Wifi
    .doc = What one code says, read out of its payload.
    .noun = { $count ->
        [one] wifi
       *[other] wifis
    }

d-lzomebum = Ssid

d-r4cfx2eg = Auth

d-vjej4gya = Password

d-cpvlblyf = Hidden

d-4gga5k7s-filter-hidden = Hidden

d-utavtcyf = Contact
    .doc = A contact card's fields, read out of a vCard or MECARD payload.
    .noun = { $count ->
        [one] contact
       *[other] contacts
    }

d-vqb7vg46 = Name

d-m3wjb2ux = Phone

d-ozre57rz = Email

d-66g4od3j = Org

d-leo66m26 = Capture preferences
    .doc = How this person takes pictures: their choices, theirs alone, on whichever device they open the camera.
    .noun = { $count ->
        [one] capture preference
       *[other] capture preferences
    }

d-kbzf222p = Resolution
    .doc = `12mp`, `4k`, `1080p` or `720p`.

d-zqovr7j4 = Photo format
    .doc = `jpeg` or `png`.

d-qv6u7j3k = Timer
    .doc = Seconds between the shutter and the capture: 0, 3 or 10.

d-7htbqsa6 = Grid

d-xpbujo45 = Level

d-ly3zxilj = Mirror front

d-b4n265en = Geotag

d-uucfkei5 = Hdr

d-nfm4slrd = Default lens

d-cfihwt6m = Mode
    .doc = `photo` or `video`: the mode the viewfinder opens in.

d-v5w2qmvn = Lens
    .doc = The lens the viewfinder shows.

d-zaeshfry = Device
    .doc = The camera this device uses, by its node; empty asks the platform's default.

d-ffavn4zg = Preview
    .doc = The newest frame the camera showed, kept as a picture.

d-b4frgf3q = Stream
    .doc = The camera's moving picture, by its engine stream.

d-s45visew = Found
    .doc = The first code the newest frame holds, and where it is in it.

d-v7ncmrtp = Found x

d-frttwlrr = Found y

d-di5cyk72 = Found w

d-3adwwuks = Found h

d-fnu3wna2 = Recording
    .doc = The clip being recorded, by its engine job.

d-bkfl557v = Shutter at
    .doc = The moment the self-timer fires, while it counts down.

d-zu2gt2mk = Trouble
    .doc = Why the last capture, recording or look failed.

d-leo66m26-filter-timer = Timer

d-leo66m26-filter-grid = Grid

d-leo66m26-filter-level = Level

d-leo66m26-filter-mirror_front = Mirror front

d-leo66m26-filter-geotag = Geotag

d-leo66m26-filter-hdr = Hdr

d-leo66m26-filter-default_lens = Default lens

d-leo66m26-filter-lens = Lens

d-leo66m26-filter-found_x = Found x

d-leo66m26-filter-found_y = Found y

d-leo66m26-filter-found_w = Found w

d-leo66m26-filter-found_h = Found h

d-leo66m26-filter-shutter_at = Shutter at

d-czua3iyv = Scanned page
    .doc = One scanned page: its picture, the text read off it, and where it stands in the document.
    .noun = { $count ->
        [one] scanned page
       *[other] scanned pages
    }

d-5tzdwbr2 = File

d-noe6wvhn = Text

d-a6zvslgh = Order

d-czua3iyv-filter-order = Order

d-tqduxezj = Scan session
    .doc = The document being scanned: this person's own, so a half-finished document never reaches anyone else.
    .noun = { $count ->
        [one] scan session
       *[other] scan sessions
    }

d-r3wtymsq = Scanner
    .doc = The scanner's device node; empty until one is chosen.

d-7wujh6qk = Resolution

d-mmi6goha = Colour
    .doc = `color`, `grayscale` or `lineart`.

d-jmjue7md = Source
    .doc = `flatbed`, `feeder` or `feeder-duplex`.

d-auvo6snv = Language
    .doc = `eng`, `deu`, `fra` or `spa`.

d-pgl6ysyg = Pages

d-2yr4wc6x = Job
    .doc = The scan running now, by its engine job.

d-7mup54lj = Failed

d-yn4bzy6x = Selected

d-tqduxezj-filter-resolution = Resolution

d-tqduxezj-filter-selected = Selected

d-gqlwtb4p = Camera
    .doc = The camera's own resource: the codes it scanned, the capture it hands on, and each person's preferences and document.
    .noun = { $count ->
        [one] camera
       *[other] cameras
    }

d-mbpsgka6 = Scans

d-rx3qy2lc = Recent

d-vdbaklqk = Preferences

d-5q2zbyut = Sessions

d-gqlwtb4p-filter-recent = Recent

d-5tikikdw = Still ask
    .noun = { $count ->
        [one] still ask
       *[other] still asks
    }

d-tbd3ic3b = Device

d-ijab2zhm = Width

d-zyjznh7y = Height

d-w2ygpawb = Format

d-5tikikdw-filter-width = Width

d-5tikikdw-filter-height = Height

d-hms7afma = Frame ask
    .noun = { $count ->
        [one] frame ask
       *[other] frame asks
    }

d-ycmsih2o = Device

d-yvbln5ns = Width

d-kzl4t5dn = Height

d-hms7afma-filter-width = Width

d-hms7afma-filter-height = Height

d-yo6tepfn = Codes ask
    .noun = { $count ->
        [one] codes ask
       *[other] codes asks
    }

d-vzfp47qk = Url

d-v7y3lgcv = Width

d-yo6tepfn-filter-width = Width

d-o7ofgj4m = Decoded code
    .noun = { $count ->
        [one] decoded code
       *[other] decoded codes
    }

d-fzwv4d4j = Text

d-lsk2z7xu = X

d-ukafx4ho = Y

d-hqwha6oq = W

d-mc2k2zbw = H

d-o7ofgj4m-filter-x = X

d-o7ofgj4m-filter-y = Y

d-o7ofgj4m-filter-w = W

d-o7ofgj4m-filter-h = H

d-jydnj6jf = Codes answer
    .noun = { $count ->
        [one] codes answer
       *[other] codes answers
    }

d-etfbstke = Codes

d-j7b7gllt = Camera device
    .noun = { $count ->
        [one] camera device
       *[other] camera devices
    }

d-cowbxfqn = Id

d-umm5rpj4 = Label

d-teae7rnw = Cameras answer
    .noun = { $count ->
        [one] cameras answer
       *[other] cameras answers
    }

d-3sxueu6f = Devices

d-rnqfpsmo = Prompt

d-teae7rnw-filter-prompt = Prompt

d-bvcsvkpx = Job answer
    .noun = { $count ->
        [one] job answer
       *[other] job answers
    }

d-3uzmfe3a = Job

d-v3o2ntdi = Record ask
    .noun = { $count ->
        [one] record ask
       *[other] record asks
    }

d-zm52cba6 = Device

d-2vmgzw6b = Width

d-2oehkbmq = Height

d-v3o2ntdi-filter-width = Width

d-v3o2ntdi-filter-height = Height

d-e35oscgd = Record ask job
    .noun = { $count ->
        [one] record ask job
       *[other] record ask jobs
    }

d-vkx7nslm = Job

d-qkssv4e7 = Record stopped
    .noun = { $count ->
        [one] record stopped
       *[other] record stoppeds
    }

d-tm6dvkh2 = Video

d-qkssv4e7-filter-video = Video

d-saucu6xa = Recorded clip
    .noun = { $count ->
        [one] recorded clip
       *[other] recorded clips
    }

d-tzmimu3l = Name

d-7znademx = Duration ms

d-iwtkarql = W

d-zmsyrjzf = H

d-zgalivtz = Size

d-saucu6xa-filter-duration_ms = Duration ms

d-saucu6xa-filter-w = W

d-saucu6xa-filter-h = H

d-saucu6xa-filter-size = Size

d-jsvqxuiu = Scan ask
    .noun = { $count ->
        [one] scan ask
       *[other] scan asks
    }

d-3m6nff75 = Scanner

d-o4hag5xq = Resolution

d-hvk7vnxj = Colour

d-7zqczcaa = Source

d-sq2qwcuy = Name

d-jsvqxuiu-filter-resolution = Resolution

d-oekiybf5 = Ocr ask
    .noun = { $count ->
        [one] ocr ask
       *[other] ocr asks
    }

d-35d3ajgx = Url

d-tlhgxi3u = Language

d-2h535m6b = Ocr answer
    .noun = { $count ->
        [one] ocr answer
       *[other] ocr answers
    }

d-3hu3iumn = Text

d-k2lbevvr = Words

d-nof45z64 = Deskew

d-2h535m6b-filter-words = Words

d-2h535m6b-filter-deskew = Deskew

d-cubz27yy = Pdf page
    .noun = { $count ->
        [one] pdf page
       *[other] pdf pages
    }

d-siogqny4 = Url

d-anpoq2j7 = Text

d-ueo4cdf2 = Pages ask
    .noun = { $count ->
        [one] pages ask
       *[other] pages asks
    }

d-mmydep5v = Pages

d-rclpsfrb = Name

d-ya3wmmea = Url answer
    .noun = { $count ->
        [one] url answer
       *[other] url answers
    }

d-llffazxv = Url

d-msb5dvm7 = Facing
    .doc = Which lens the viewfinder shows: the rear one, one facing the person, or the wide one.

d-b6pdb7v5 = Back

d-jeqrusvc = Front

d-xpkzvgql = Wide

d-h536fbrs = Capture kind
    .doc = A capture is a still picture or a clip.

d-hx6uqtht = Photo

d-4g3qth5v = Video

d-qxmqpf5k = Register capture
    .doc = Takes what the engine kept as the capture the app hands on.

d-qxmqpf5k-capture = Capture

d-7kultry2 = Record scan
    .doc = Keeps a scanned code in the history; the same code seen again moves to the front rather than repeating.

d-7kultry2-raw = Raw

d-t6bp2sdr = Set grid
    .doc = Shows the composition grid, or takes it away.

d-t6bp2sdr-on = On

d-vkgfmufj = Set level
    .doc = Shows the level guide, or takes it away.

d-vkgfmufj-on = On

d-jdd5qaoa = Prefs of
    .doc = The person's preferences, made the first time they are written.

d-ne7yftmp = Session of
    .doc = The person's document, made the first time it is written.

d-lgezlkbg = Use mode
    .doc = Opens the viewfinder.

d-lgezlkbg-mode = Mode

d-buzhuymc = Use lens
    .doc = Shows one lens.

d-buzhuymc-lens = Lens

d-m2z6cfgd = Cycle lens
    .doc = Switches to the next lens.

d-2tcmszvm = Use camera
    .doc = Uses the camera at this device node.

d-2tcmszvm-device = Device

d-s44o53wf = Retry camera
    .doc = Forgets the chosen camera and asks the engine for the cameras again.

d-5ehf64gl = Set resolution

d-5ehf64gl-resolution = Resolution

d-63muyhu2 = Set photo format

d-63muyhu2-format = Format

d-ffke525n = Set timer

d-ffke525n-seconds = Seconds

d-rzzflp6r = Set mirror front

d-rzzflp6r-on = On

d-grq56gsw = Set geotag

d-grq56gsw-on = On

d-pcrzoij6 = Set hdr

d-pcrzoij6-on = On

d-pzlmrbni = Set default lens

d-pzlmrbni-lens = Lens

d-o6smwxgu = Start look
    .doc = Starts the camera's moving picture: the window paints the newest frame as it arrives.

d-cigvdjta = Stop look
    .doc = Stops the camera's moving picture, letting the device go.

d-2sycocxn = Look
    .doc = Reads the codes the newest frame holds, keeping the first one the scanner view shows.

d-md4bq2ap = Read codes
    .doc = Decodes the codes in the picture at `url`, keeping the first one the scanner view shows.

d-md4bq2ap-url = Url

d-ite7lhg2 = Shutter
    .doc = The shutter: takes a photo now, starts the self-timer, or records.

d-g4pcikmm = Arm timer
    .doc = Arms the self-timer the person chose, or cancels the one counting down.

d-bb47r4i7 = Capture
    .doc = Takes a photo of the frame the camera shows, and hands it on.

d-xu77d7u4 = Start recording
    .doc = Starts recording from the frames the camera's engine thread takes.

d-5fj2zyp3 = Recorded
    .doc = The recording ended: the clip it made and handed on, or why it made none.

d-5fj2zyp3-j = J

d-hl65pw5y = Stop recording
    .doc = Stops the clip recording and hands it on once the engine has muxed it.

d-oujb6up2 = Capture page
    .doc = Captures one page with the chosen scanner and settings.

d-s4rr32oh = Scanned
    .doc = The scan ended: its page added to the document, or why none came.

d-s4rr32oh-j = J

d-codxq4tq = Shutter due
    .doc = The self-timer's moment came: take the photo it counted down to, unless it was cancelled.

d-ehd7gfhp = Look on open
    .doc = Opening the camera starts its moving picture and reads what it sees.

d-el5ehp2f = Import image
    .doc = Takes a chosen image as the code to read, without importing it.

d-el5ehp2f-file = File

d-c2iymz7f = Copy code
    .doc = Copies what the scanned code says, or its password or link.

d-c2iymz7f-which = Which

d-e4vgiklk = Keep scan
    .doc = Keeps the scanned code in the history.

d-cupovqjz = Set scanner
    .doc = Chooses the scanner the document scans with.

d-cupovqjz-device = Device

d-oamdoyiw = Set scan resolution

d-oamdoyiw-resolution = Resolution

d-dhunhdy5 = Set scan colour

d-dhunhdy5-colour = Colour

d-oy7e76at = Set scan source

d-oy7e76at-source = Source

d-zqkxzxd6 = Set ocr language

d-zqkxzxd6-language = Language

d-jnyvgsk5 = Select page

d-jnyvgsk5-page = Page

d-reqcqfow = Rotate page
    .doc = Turns the selected page a quarter clockwise; its text is read again when the person asks.

d-zp3nn5pz = Recognize text
    .doc = Reads the text off the selected page in the chosen language.

d-tvrbfrqk = Copy text
    .doc = Copies the selected page's recognized text.

d-whmjg47z = Move page
    .doc = Moves the selected page one place earlier, or later.

d-whmjg47z-later = Later

d-3oinja4j = Remove page
    .doc = Throws the selected page away.

d-p4lqzeb5 = Crop source
    .doc = The selected page's picture, as the crop frame works on it.

d-apafql4c = Crop page
    .doc = Crops the selected page to the part the person kept; its text is read again when they ask.

d-apafql4c-left = Left

d-apafql4c-top = Top

d-apafql4c-right = Right

d-apafql4c-bottom = Bottom

d-77rgf4zh = Document pdf
    .doc = The document as one searchable PDF: pages with recognized text carry it, the rest are pictures alone.

d-7fzyy7qw = Open recent
    .doc = Opens the capture that was taken last in the app that owns its bytes.

d-qtwnyshy = Open photos
    .doc = Opens the library the captures go to.

d-iwjcok7m = Open camera privacy
    .doc = Opens this device's camera privacy settings.

d-mim6ysmz = Viewfinder
    .w-photo = Photo
    .w-video = Video
    .w-scan = Scan
    .w-documents = Documents
    .w-photos = Photos
    .w-settings = Settings
    .w-the-viewfinder = The viewfinder
    .w-recording = Recording
    .w-grid = Grid
    .w-level = Level
    .w-hdr = HDR
    .w-latest-capture = Latest capture
    .w-clip = Clip
    .w-preferences = Preferences
    .w-this-device-s-camera = This device's camera: { $device_words } · { $resolution } · { $photo_format }

d-bclc7wcv = Scanner page
    .w-photo = Photo
    .w-scan = Scan
    .w-documents = Documents
    .w-scan-an-image = Scan an image
    .w-look-again = Look again
    .w-what-the-camera-shows = What the camera shows
    .w-no-picture-yet = No picture yet
    .w-look-through-the-camera-or-scan = Look through the camera, or scan an image you already have.
    .w-detected-code = Detected code
    .w-keep-in-history = Keep in history
    .w-recent-codes = Recent codes

d-c2oiqsed = Documents page
    .w-photo = Photo
    .w-scan = Scan
    .w-documents = Documents
    .w-no-scanner-found = No scanner found
    .w-connect-a-document-scanner-to-this = Connect a document scanner to this computer, then open this page again.
    .w-scanner = Scanner
    .w-resolution = Resolution
    .w-dpi = { $dpi } dpi
    .w-colour = Colour
    .w-grayscale = Grayscale
    .w-line-art = Line art
    .w-paper-source = Paper source
    .w-flatbed = Flatbed
    .w-feeder = Feeder
    .w-feeder-both-sides = Feeder, both sides
    .w-text-language = Text language
    .w-english = English
    .w-german = German
    .w-french = French
    .w-spanish = Spanish
    .w-the-scanner-is-working = The scanner is working
    .w-no-pages-yet = No pages yet
    .w-scanned-pages-appear-here = Scanned pages appear here.
    .w-pages = Pages
    .w-text-not-recognized = Text not recognized
    .w-words = { $words } words
    .w-scan-page = Scan page
    .w-turn = Turn
    .w-earlier = Earlier
    .w-later = Later
    .w-remove-page = Remove page
    .w-crop = Crop
    .w-apply-crop = Apply crop
    .w-cancel-crop = Cancel crop
    .w-recognize-text = Recognize text
    .w-copy-text = Copy text
    .w-save-as-pdf = Save as PDF

d-iswgtf4f = Settings page
    .w-back-to-the-camera = Back to the camera
    .w-capture = Capture
    .w-resolution = Resolution
    .w-photo-format = Photo format
    .w-jpeg = JPEG
    .w-png = PNG
    .w-self-timer = Self-timer
    .w-off = Off
    .w-3-seconds = 3 seconds
    .w-10-seconds = 10 seconds
    .w-default-camera = Default camera
    .w-rear = Rear
    .w-front = Front
    .w-ultra-wide = Ultra-wide
    .w-viewfinder-and-photos = Viewfinder and photos
    .w-hdr = HDR
    .w-use-hdr = Use HDR
    .w-rule-of-thirds-grid = Rule-of-thirds grid
    .w-show-the-grid = Show the grid
    .w-level-guide = Level guide
    .w-show-the-level = Show the level
    .w-mirror-front-camera = Mirror front camera
    .w-mirror-front-camera-photos = Mirror front-camera photos
    .w-save-location-with-photos = Save location with photos
    .w-tag-photos-with-where-they-were = Tag photos with where they were taken
