# Displays pane: arrangement canvas
display-arrangement-title = Arrangement
display-arrangement-name = Display arrangement
display-arrangement-description = Press a display to choose it and drag it to place it beside another. From the keyboard, the display picker chooses one and the arrow keys move it.
display-arrangement-empty = No displays to arrange
display-arrangement-item = Display { $number }: { $name }
display-arrangement-item-primary = Display { $number }: { $name }, main display
display-arrangement-item-position = { $width } × { $height } at { $x }, { $y }
display-identify = Identify
display-resolution = Resolution
display-refresh-rate = Refresh rate
display-modes-unavailable = Not reported by this display

# Displays pane: layout tools trusted to change displays without asking
display-tools-title = Display tools
display-tools-caption = These programs can make risky display changes without asking you to keep them.
display-tools-forget = Ask again
display-tools-forgotten = Risky display changes from this tool will ask to be kept again.

# Displays pane: HDR
display-hdr = HDR
display-hdr-description = Brighter highlights and richer colour for HDR games, video and photos
display-hdr-sdr-brightness = SDR content brightness
display-hdr-sdr-brightness-description = How bright everything that is not HDR looks while HDR is on
display-hdr-unsupported = Not supported by this display
display-hdr-driver-unsupported = The graphics driver cannot send an HDR signal to this display
display-hdr-format-unsupported = The graphics card could not show a 10-bit picture on this display
display-hdr-driver-rejected = The graphics driver refused to turn on HDR, so this display stays in SDR

# Displays pane: variable refresh rate
display-vrr = Variable refresh rate
display-vrr-description = Turns on by itself for fullscreen games and video
display-vrr-unsupported = Not supported by this display
display-vrr-driver-unsupported = The graphics driver does not offer it for this display
display-vrr-driver-rejected = The graphics driver refused to turn it on

# Displays pane: a connected display that could not be lit
display-status = Status
display-link-down = No picture reached this display: the connection to it did not come up. Reconnect the cable to try again
display-output-driver-rejected = The graphics driver refused to turn this display on

# Displays pane: night light, rotation, built-in name
display-builtin = Built-in display
display-brightness = Brightness
display-brightness-dimmed-by-tilo = Dimmed by Tilo. The monitor’s own brightness stays the same.
display-auto-brightness = Automatic brightness
display-no-ambient = No ambient-light sensor detected
display-night-light = Night light
display-night-light-schedule = Sunset to sunrise
display-night-light-schedule-desc = Turn night light on and off on the Appearance day and night schedule
display-night-light-warmth = Night light warmth
display-night-light-temp-subtle = Subtle (4500 K)
display-night-light-temp-warm = Warm (3700 K)
display-night-light-temp-very-warm = Very warm (2800 K)
display-rotation = Rotation
display-rotation-landscape = Landscape
display-rotation-portrait-right = Portrait right
display-rotation-upside-down = Upside down
display-rotation-portrait-left = Portrait left

display-none = No displays detected on this device.
display-unreadable = This device’s displays can’t be read right now.
display-use = Use this display
display-primary = Main display
display-primary-marker = Main display
display-mode-label = { $width } × { $height }, { $hertz } Hz
display-scale = Scale
display-mirroring = Mirroring
display-colour-profile = Colour profile
display-colour-profile-empty = System default
display-arrangement-count = { $count ->
    [one] One display
   *[other] { $count } displays
}
display-keep-title = Keep these display settings?
display-keep-caption = The previous settings come back in { $seconds } s
display-keep = Keep
display-revert-now = Revert
display-applying-title = Applying display settings
display-applying-caption = Applying…
display-rejected-caption = These display settings couldn’t be applied.
display-retry = Try the changes again
display-try-again = Try again
display-rejected-title = Display settings not applied
display-apply-title = Display changes to try
display-apply-caption = Resolution, refresh rate, mirroring, HDR and turning displays on or off are tried for 15 seconds before they stay.
display-apply-row = Try the new settings
display-apply = Try

# Mouse & Keyboard pane: touchpad
input-touchpad-title = Touchpad
input-touchpad-enabled = Touchpad
input-touchpad-enabled-description = Turn off to ignore the touchpad; the touchpad key does the same
input-touchpad-disable-with-mouse = Disable touchpad while a mouse is connected
input-touchpad-disable-with-mouse-description = The touchpad comes back when the mouse is unplugged
input-touchpad-speed = Touchpad speed
input-touchpad-natural-scroll = Natural scrolling
input-touchpad-natural-scroll-description = Content tracks finger movement
input-tap-to-click = Tap to click
input-tap-to-click-description = Tap the touchpad to register a click
input-tap-drag = Tap-and-drag
input-tap-drag-description = Hold after a tap, then move to drag
input-tap-drag-lock = Drag lock
input-tap-drag-lock-description = A tap-and-drag continues after you lift your finger, until the next tap
input-disable-while-typing = Disable while typing
input-disable-while-typing-description = Prevents accidental palm input
input-touchpad-gestures = Touchpad gestures
input-touchpad-gestures-description = Three-finger navigation; apps still receive other gestures

# Mouse & Keyboard pane: touchpad clicking and scrolling
input-clicking-title = Clicking & scrolling
input-click-method = Secondary click
input-click-method-default = Device default
input-click-method-areas = Bottom-right corner
input-click-method-clickfinger = Two-finger press
input-click-method-caption = Bottom-right corner: press there to right-click. Two-finger press: press with two fingers to right-click, three to middle-click.
input-scroll-method = Scrolling
input-scroll-method-two-finger = Two fingers
input-scroll-method-edge = Along the edge

# Mouse & Keyboard pane: mouse
input-mouse-title = Mouse
input-mouse-speed = Mouse speed
input-mouse-natural-scroll = Natural scrolling
input-mouse-natural-scroll-description = Content moves in the wheel direction

# Mouse & Keyboard pane: every pointer
input-pointer-title = Buttons & acceleration
input-left-handed = Left-handed
input-left-handed-description = Swap primary and secondary buttons
input-middle-emulation = Middle-button emulation
input-middle-emulation-description = Press left and right together for middle-click
input-acceleration = Acceleration
input-scroll-speed = Scroll speed
input-scroll-speed-description = Distance per wheel click or finger movement

# Mouse & Keyboard pane: drawing tablets
input-tablet-whole-desktop = Whole desktop
input-tablet-map-to = Map to
input-tablet-keep-aspect = Keep aspect ratio
input-tablet-keep-aspect-description = Use only the part of the tablet that matches the display's shape, so strokes are not stretched
input-tablet-left-handed = Left-handed orientation
input-tablet-left-handed-description = Turn the tablet around, with its buttons on the right
input-tablet-tip-feel = Tip feel
input-tablet-tip-feel-description = Soft draws heavily with a light touch; firm needs a harder press
input-tablet-lower-button = Lower pen button
input-tablet-upper-button = Upper pen button
input-tablet-third-button = Third pen button
input-tablet-third-button-description = On pens that have one
input-tablet-eraser = Eraser
input-tablet-eraser-description = What the other end of the pen does
input-eraser-native = Erase
input-pen-button-default = Default
input-pen-button-left = Left click
input-pen-button-middle = Middle click
input-pen-button-right = Right click
input-pen-button-back = Back
input-pen-button-forward = Forward
input-pen-button-disabled = Do nothing
input-pad-title = { $name } buttons
input-pad-button = Pad button { $number }
input-pad-button-none = Do nothing
input-pad-button-show-map = Show what the buttons do
input-pad-no-buttons = This pad reports no buttons
input-pad-mode-button-description = Switches the rings, strips and buttons to their next mode
input-pad-ring = Ring { $number }
input-pad-ring-mode = Ring { $number }, mode { $mode }
input-pad-strip = Strip { $number }
input-pad-strip-mode = Strip { $number }, mode { $mode }
input-pad-dial-nothing = Do nothing
input-pad-dial-scroll = Scroll
input-pad-dial-zoom = Zoom
input-pad-dial-brush-size = Brush size
input-pad-dial-volume = Volume
input-pad-dial-brightness = Brightness
input-pad-dials-caption = Drawing apps that support tablets get the rings and strips directly

# Keyboard shortcuts pane: the desktop shortcut catalogue, named as the shell names it
shortcut-screenshot = Screenshot
shortcut-switch-windows = Switch windows
shortcut-switch-windows-backwards = Switch windows backwards
shortcut-switch-app-windows = Switch windows of the app
shortcut-close-tab = Close tab
shortcut-change-workspace = Change workspace
shortcut-new-window = New window
shortcut-duplicate-tab = New tab of this app
shortcut-open-terminal = Open terminal
shortcut-strip-switcher = Strip switcher
shortcut-display-layout = Display layout
shortcut-capture-screenshot = Capture screenshot
shortcut-picture-in-picture = Picture in picture
shortcut-save-recent-replay = Save recent replay
shortcut-workspace-history = Workspace history
shortcut-explore-workspace-history = Explore workspace history
shortcut-guide = Keyboard shortcuts
shortcut-stop-remote-input = Stop all remote input
shortcut-toggle-mouse-keys = Toggle mouse keys
shortcut-name-search = Search everywhere
shortcut-name-workspace-overview = Overview
shortcut-name-lock = Lock screen
shortcut-name-quick-settings = Quick Settings
shortcut-name-shortcut-help = Keyboard shortcuts
shortcut-name-focus-previous-window = Focus previous window
shortcut-name-focus-next-window = Focus next window
shortcut-name-previous-tab = Previous tab
shortcut-name-next-tab = Next tab
shortcut-name-previous-strip = Strip above
shortcut-name-next-strip = Strip below
shortcut-name-previous-workspace = Previous workspace
shortcut-name-next-workspace = Next workspace
shortcut-name-toggle-maximize = Maximize or restore window
shortcut-name-maximize-window = Maximize window
shortcut-name-restore-window = Restore window
shortcut-name-detach-tab = Move tab to new group
shortcut-name-attach-tab-left = Move tab into group on left
shortcut-name-attach-tab-right = Move tab into group on right
shortcut-name-move-group-left = Move group left
shortcut-name-move-group-right = Move group right
shortcut-name-move-group-to-previous-strip = Move group to strip above
shortcut-name-move-group-to-next-strip = Move group to strip below
shortcut-name-move-group-to-previous-display = Move group to display on left
shortcut-name-move-group-to-next-display = Move group to display on right
shortcut-name-resize-narrower = Make window narrower
shortcut-name-resize-wider = Make window wider
shortcut-name-reopen-closed-tab = Reopen closed tab
shortcut-name-focus-group-1 = Window 1
shortcut-name-focus-group-2 = Window 2
shortcut-name-focus-group-3 = Window 3
shortcut-name-focus-group-4 = Window 4
shortcut-name-focus-group-5 = Window 5
shortcut-name-focus-group-6 = Window 6
shortcut-name-focus-group-7 = Window 7
shortcut-name-focus-group-8 = Window 8
shortcut-name-focus-group-9 = Window 9
shortcut-name-focus-strip-1 = Strip 1
shortcut-name-focus-strip-2 = Strip 2
shortcut-name-focus-strip-3 = Strip 3
shortcut-name-focus-strip-4 = Strip 4
shortcut-name-focus-strip-5 = Strip 5
shortcut-name-focus-strip-6 = Strip 6
shortcut-name-focus-strip-7 = Strip 7
shortcut-name-focus-strip-8 = Strip 8
shortcut-name-focus-strip-9 = Strip 9
shortcut-name-open-files = Open Files
shortcut-name-open-terminal = Open a terminal
shortcut-name-clipboard-history = Clipboard history
shortcut-name-notifications = Notifications
shortcut-name-toggle-fullscreen = Full screen
shortcut-name-toggle-magnifier = Magnifier
shortcut-name-magnifier-zoom-in = Magnifier zoom in
shortcut-name-magnifier-zoom-out = Magnifier zoom out
shortcut-name-power-menu = Power menu
shortcut-name-next-input-source = Next keyboard layout
shortcut-name-emoji-picker = Emoji
shortcut-name-say-something = Say something here
shortcut-name-ask-agent = Ask agent about selection
shortcut-category-system = System
shortcut-category-navigation = Navigation
shortcut-category-windows = Windows
shortcut-category-apps = Apps
shortcut-category-capture = Capture
shortcut-category-accessibility = Accessibility
settings-shortcut-unassigned = Unassigned
settings-shortcuts-vim = Vim navigation (H J K L)
settings-shortcuts-app-tab-chords = { $next }, { $previous } and { $reopen } reach the app first and switch or reopen tabs only when the app doesn’t use them. The shortcuts for Next tab, Previous tab and Reopen closed tab always do.
settings-paste-title = Paste in apps
settings-paste-choose = Add App…
settings-paste-choose-purpose = set how it pastes
settings-paste-no-chooser = Apps can be chosen only in a Tilo session
settings-paste-shortcut-for = How { $app } pastes
settings-paste-remove = Remove { $app }
settings-shortcuts-actions = Actions
settings-shortcuts-actions-caption = A saved shortcut’s keyboard trigger binds its chord here; change or remove it in the shortcut.

# Devices pane: what each Continuity switch shares
continuity-clipboard-description = Share copied text and images between your devices.
continuity-notifications-description = Show this device's notifications on your other devices.
continuity-wifi-settings-description = Share saved Wi-Fi networks and personal settings.
continuity-media-description = Continue playback on another device and play through its speakers.
continuity-display-input-description = Use another device as an extra display, keyboard and mouse.
continuity-file-transfer-description = Send files between your devices.
continuity-realtime-sessions-description = Hand off calls and other live sessions.
continuity-typed-gamepad-description = Use a game controller from another device.
continuity-app-continuations-description = Pick up where you left off in an app on another device.

# Speakers pane
speakers-overview-title = Speakers across your devices
speakers-overview-empty = Use speakers on several of your devices together: a stereo pair, surround, or speakers around the room.
speakers-media-off = Turn on Media in Devices & sync to play through speakers on several of your devices.
speakers-begin = Set up speakers
speakers-member-summary = { $device } ({ $role })
speakers-ready-count = { $ready } of { $total } ready
speakers-set-up-elsewhere = { $summary }, set up on { $leader }
speakers-clock-failed-detail = { $summary }, can't play in time: { $reason }
speakers-calibrate-button = Calibrate
speakers-delete = Delete
speakers-delete-arm = Delete…
speakers-back = Back
speakers-continue = Continue
speakers-done = Done
speakers-save = Save
speakers-discard = Discard
speakers-apply = Apply
speakers-revert = Revert
speakers-try-again = Try again
speakers-arrangement = Arrangement
speakers-arrangement-stereo = Stereo pair
speakers-arrangement-surround = Surround
speakers-arrangement-room = Around the room
speakers-arrangement-stereo-description = Two speakers play left and right
speakers-arrangement-surround-description = Three to six speakers in named positions
speakers-arrangement-room-description = Speakers placed anywhere around you
speakers-choose-title = Choose speakers
speakers-no-outputs = No speakers are available. Turn on Media in Devices & sync on each device you want to use.
speakers-output-this-device = This device
speakers-output-asks = Asks for approval on that device when you save
speakers-output-use = Use { $label }
speakers-role-left = left speaker
speakers-role-right = right speaker
speakers-role-center = centre speaker
speakers-role-lfe = subwoofer
speakers-role-surround-left = left surround speaker
speakers-role-surround-right = right surround speaker
speakers-role-room = room speaker
speakers-answer-left = Left
speakers-answer-right = Right
speakers-answer-center = Centre
speakers-answer-lfe = Subwoofer
speakers-answer-surround-left = Left surround
speakers-answer-surround-right = Right surround
speakers-answer-room = Room
speakers-member-ready = Ready
speakers-member-awaiting-consent = Waiting for approval on that device
speakers-member-offline = Offline
speakers-member-output-missing = Output unplugged
speakers-member-unavailable = Media sharing is off there
speakers-member-not-certified = Not one of your devices
speakers-member-role-trim = { $role }, { $trim }
speakers-playback-playing = Playing
speakers-playback-connecting = Connecting…
speakers-playback-syncing = Syncing clocks…
speakers-playback-clock-failed = Can't play in time
speakers-identify-title = Which one just played?
speakers-identify-intro = Speaker { $index } of { $count }. Play a sound and listen for which speaker it comes from.
speakers-play-sound = Play sound
speakers-identify-room-next = You will place it on the room layout next; each speaker starts on an arc in front of you.
speakers-next-speaker = Next speaker
speakers-identify-answer = It was the { $role }
speakers-identify-play-first = Play the sound first.
speakers-assign-listen-first = listen to a speaker first
speakers-assign-room-on-layout = place room speakers on the layout
speakers-assign-taken = another speaker is already the { $role }
speakers-problem-stereo-too-few = Choose two speakers for a stereo pair
speakers-problem-surround-too-few = Choose at least three speakers for surround
speakers-problem-room-too-few = Choose at least one speaker
speakers-problem-stereo-too-many = A stereo pair has exactly two speakers
speakers-problem-surround-too-many = Surround uses at most six speakers
speakers-problem-room-too-many = Use at most eight speakers
speakers-layout-title = Room layout
speakers-layout-intro = Drag each speaker to where it stands. You sit at the marker.
speakers-layout-speaker = Speaker
speakers-layout-position = Position
speakers-layout-position-value = { $across }, { $depth }
speakers-layout-across-left = { $metres } m to your left
speakers-layout-across-right = { $metres } m to your right
speakers-layout-across-centre = Centred
speakers-layout-depth-front = { $metres } m in front of you
speakers-layout-depth-behind = { $metres } m behind you
speakers-layout-depth-level = level with you
speakers-layout-identify = Identify
speakers-layout-play-on = Play a sound on { $label }
speakers-layout-crowded = Two speakers cannot stand in exactly the same spot
speakers-room-you = You
speakers-room-name = Speakers in the room
speakers-name-title = Name your speakers
speakers-name = Name
speakers-name-description = Shown wherever you can play to these speakers
speakers-name-placeholder = Living room
speakers-default-name = Speakers
speakers-name-empty = Give the speakers a name
speakers-name-too-long = Use a shorter name
speakers-name-unidentified = Tell which speaker { $speaker } is
speakers-saving = Saving your speakers…
speakers-saved-title = Speakers saved
speakers-saved-message = Saved. Your other devices ask once whether to join; approve there, then calibrate here.
speakers-calibrate-title = Calibrate { $name }
speakers-calibrate-gone = That speaker set is gone
speakers-calibrate-elsewhere = Calibrate { $name } on { $leader }, where it was set up
speakers-calibrate-member-not-ready = { $device }: { $status }
speakers-trim-change = Before { $before }, after { $after }
speakers-service-unreachable = Could not reach the speaker service: { $error }
speakers-service-unexpected = the speaker service answered unexpectedly
speakers-done-created = Speaker set { $name } created
speakers-done-identify = Playing a sound
speakers-done-calibrate = Calibrating
speakers-done-apply = Calibration applied
speakers-done-revert = Previous calibration restored
speakers-done-dismiss = Calibration discarded
speakers-done-delete = Speaker set deleted

# Speakers pane: calibration from where you sit
speakers-listening-title = Where you sit
speakers-listening-description = Each speaker plays a sweep in turn; this microphone hears when it arrives where you sit.
speakers-also-listen-caption = Also listen with
speakers-microphone-this-device-title = { $label } (this device)
speakers-microphone-use = Also listen with { $label }
speakers-microphone-role-member = Measures that speaker’s own latency
speakers-microphone-role-cross-check = A cross-check from elsewhere in the room
speakers-microphone-this-device = This device
speakers-microphone-ready = Allowed on that device
speakers-microphone-not-asked = Asks for approval on that device when you start
speakers-microphone-awaiting = Waiting for approval on that device
speakers-microphone-declined = Declined on that device
speakers-microphone-offline = Offline
speakers-microphone-unavailable = Media sharing is off there
speakers-calibrate-no-microphone = None of your devices offers a microphone to calibrate with
speakers-calibrate-already-running = Calibration is already running
speakers-calibration-cancel = Cancel
speakers-calibration-cancelled = Calibration cancelled
speakers-run-aligned-for = Aligned for { $label }
speakers-run-member-waiting = Waiting
speakers-run-member-measuring = Listening…
speakers-run-member-arrives = Arrives after { $delay } ms at your seat
speakers-run-member-output-latency = its output takes { $latency } ms
speakers-run-member-distance = about { $metres } m away
speakers-run-microphone-listening-position = { $label } (where you sit)
speakers-run-microphone-awaiting = Waiting for approval on that device
speakers-run-microphone-declined = Declined on that device
speakers-run-microphone-listening = Listening…
speakers-run-microphone-used = Heard { $heard } speakers
speakers-run-microphone-used-rejected = Heard { $heard } speakers, { $rejected } readings set aside as inconsistent
speakers-run-microphone-not-heard = Heard nothing: too far away
speakers-run-microphone-failed = Could not listen: { $reason }
speakers-run-waiting-for-microphones = Approve on each device that asks, or cancel and switch that microphone off.

# Navigation: sidebar sections, categories and the page heading
settings-nav-name = Settings categories
settings-pane-reading = Reading this device…
settings-found-title = Matching settings
settings-found-open = Open
settings-found-show = Show
settings-value-on = On
settings-value-off = Off
upd-reading = Reading this device’s updates…
perm-reading = Reading this device…
settings-section-connectivity = Connectivity
settings-section-devices = Devices
settings-section-personal = Personal
settings-section-system = System
settings-section-accounts-permissions = Accounts & permissions
settings-personal-apps-files-title = Open files and links with
settings-personal-apps-empty = No installed app opens links or file types yet.
settings-personal-apps-ext-files = { $ext ->
    [html] Web pages
    [htm] Web pages
    [jpg] JPEG images
    [jpeg] JPEG images
    [png] PNG images
    [gif] GIF images
    [webp] WebP images
    [svg] SVG images
    [pdf] PDF documents
    [md] Markdown documents
    [txt] Text files
    [rs] Source code
   *[other] { $ext } files
 }
settings-personal-apps-scheme-links = { $scheme ->
    [http] Web links
    [https] Secure web links
    [mailto] Email links
    [tel] Phone links
    [webcal] Calendar links
   *[other] { $scheme } links
 }

# Users pane
settings-users-error-title = Could not apply that change
settings-users-empty-accounts = No regular login accounts found on this device.
settings-users-login-accounts = Login accounts
settings-users-empty-identities = No signing identity yet. You set one up in your own session.
settings-users-identities-title = Your Tilo identities
settings-users-protection-broker-file = Some keys sit in a private file that is not encrypted. Unlock your keyring to protect them.
settings-users-protection-service = Protected by your keyring
settings-users-protection-file = Stored in a private file
settings-users-protection-hardware-backed = Protected by hardware
settings-users-protection-legacy = Needs protecting
settings-users-protection-unavailable = Not available
settings-users-protection-device-certificate = Signs with this device's certificate
settings-users-placeholder-full-name = Full name
settings-users-placeholder-username = login-name
settings-users-placeholder-password = At least 10 characters
settings-users-create = Create
settings-users-add-form-title = Add login account
settings-users-full-name = Full name
settings-users-username = User name
settings-users-password = Password
settings-users-password-note = New accounts start as Standard.
settings-users-make-standard = Make standard
settings-users-make-administrator = Make administrator
settings-users-disable-autologin = Disable automatic login
settings-users-enable-autologin = Enable automatic login
settings-users-remove-ellipsis = Remove…
settings-users-role-administrator = Administrator
settings-users-role-standard = Standard
settings-users-identity-active = Active
settings-users-identity-available = Available
settings-users-identity-use = Use
settings-users-identity-remove = Remove identity
settings-users-identity-protect = Protect now
settings-users-account-gone = User account is no longer available.
settings-users-identity-store-unavailable = Identity store is unavailable.

# Notifications chrome. Focus clock times stay 24h identifiers.
notif-dnd = Do not disturb
notif-dnd-desc = Silence all banners and sounds until you turn it off
notif-banners-title = Banners
notif-show-banners = Show banners
notif-show-banners-desc = Pop up a banner when a notification arrives
notif-previews = Show message previews
notif-previews-desc = Reveal the message body in the banner, not just the app
notif-delivery-title = Delivery
notif-lock-screen = Show on lock screen
notif-lock-screen-desc = Show only a redacted count while the session is locked
notif-sounds-missing = Notification sounds are not available on this device
notif-play-sounds = Play sounds
notif-play-sounds-desc = Use the current desktop sound theme and audio route
notif-badges = Show notification badges
notif-badges-desc = Show unread counts on app icons
notif-focus-enabled = Scheduled quiet hours
notif-focus-enabled-desc = Hold banners and sounds automatically
notif-focus-from = From
notif-focus-until = Until
notif-focus-days = Days
notif-days-every = Every day
notif-days-weekdays = Weekdays
notif-days-weekends = Weekends
notif-mute-off = Not muted
notif-mute-1h = For 1 hour
notif-mute-8h = For 8 hours
notif-mute-24h = For 24 hours
notif-app-banners = Allow banners
notif-app-banners-desc = Override the global banner choice for this application
notif-app-previews = Show previews
notif-app-previews-desc = Allow message contents from this application
notif-app-sounds = Play sounds
notif-app-sounds-desc = Override the global sound choice for this application
notif-app-badges = Show badges
notif-app-badges-desc = Show unread counts for this app
notif-app-priority = Priority during focus
notif-app-priority-desc = Let this app break through scheduled quiet hours
notif-app-contacts = Priority contacts
notif-app-contacts-desc = Let this app recognise your favourite contacts, who can break through quiet hours
notif-app-mute = Mute temporarily
notif-app-allowed = Banners and their contents are currently allowed.
notif-app-hidden = Banners are allowed with their contents hidden.
notif-app-history = Notifications go to history without a banner.
notif-apps-title = Applications
notif-apps-empty = No app has sent a notification yet.
notif-priority-title = Priority contacts
notif-priority-source = Source
notif-priority-source-value = Favourites in Contacts

# Updates pane
mesh-role-personal = Personal device
mesh-role-active = Available while active
mesh-role-server = Always-on server
mesh-repl-demand = On demand
mesh-repl-workspace = Workspace state
mesh-repl-full = Full workspace copy
mesh-repl-workspace-desc = Open workspaces and app progress sync here without keeping workspace files.
mesh-repl-demand-desc = File names, metadata, and preview thumbnails stay ready. File contents download when opened.
mesh-repl-full-desc = Open workspaces, app progress, and full file copies stay synced on this device.
mesh-runtime-offline = Offline
mesh-runtime-online = Online
mesh-runtime-syncing = Syncing
mesh-runtime-ready = Ready
mesh-from = From { $origin }
mesh-resolved = Resolved to { $winner }; differs from { $others }{ $suffix }
mesh-and-more =  and { $n } more
mesh-wifi-row = Wi-Fi: { $ssid }
mesh-wifi-sealed = Sealed { $security } profile. { $detail }
mesh-provenance-err = Roaming provenance is unavailable; no conflict decision is shown: { $error }
mesh-revoke-retired = Retired
mesh-revoke-lost = Lost
mesh-revoke-stolen = Stolen
mesh-revoke-compromised = Compromised
mesh-last-seen-now = Offline, last seen just now
mesh-last-seen-min = Offline, last seen { $n } min ago
mesh-last-seen-h = Offline, last seen { $n } h ago
mesh-badge-syncing = Syncing
mesh-badge-online = Online
mesh-badge-checking = Checking
mesh-badge-removed = Removed
mesh-badge-offline = Offline
mesh-continue-title = Continue across devices
mesh-activity = Workspace and app activity
mesh-sync-auto = Sync automatically
mesh-files = Files
mesh-download-opened = Download when opened
mesh-this-device = Role
mesh-this-device-desc = Use it normally, sync while it is awake, or make it the always-on home for your workspaces and files.
mesh-home-peer = Home peer
mesh-home-ready = All selected workspace files are available here.
mesh-home-online = This device is reachable by your other devices.
mesh-home-syncing = Keep this device powered and online while file copies finish.
mesh-home-offline = The sync service is not currently reachable.
mesh-role-hint-personal = Choose Always-on server to make this device your always-on sync home.
mesh-role-hint-hybrid = Workspace updates stay available to your other devices while this device is awake.
mesh-role-hint-server-empty = Workspaces added to this device stay available around the clock.
mesh-role-hint-server-full = Open workspaces, app progress, and full file copies stay available from this device.
mesh-role-hint-server = Open workspaces and app progress stay available from this device around the clock.
mesh-workspaces-empty = Your workspaces appear here as soon as they are opened.
mesh-keep-workspace = Keep this workspace available from this device
mesh-pairing = Pairing…
mesh-approve = Approve { $label } ({ $code })
mesh-pair-home = Pair home server
mesh-add-device = Add device
mesh-personal-caption = On the new device, choose Use my existing Tilo identity. Compare the nearby six-digit code, then approve it here. Your open workspaces and trusted home peers appear automatically.
mesh-status-revoked = Revoked
mesh-status-this-device = This device
mesh-status-home-server = Home server
mesh-status-trusted = Trusted
mesh-why-ending = Why is access ending?
mesh-why-ending-desc = This reason is signed into your device history and cannot be changed later.
mesh-device-list-err = Device list unavailable: { $error }
mesh-no-target = No other device
mesh-allow-continuity = Share with your other devices
mesh-allow-continuity-desc = Turn off to stop sharing everything at once.
mesh-facet-active = { $desc } On for { $target }.
mesh-facet-prepared = { $desc } Paused while sharing is off.
mesh-facet-off = { $desc }
mesh-audit-empty = No sharing changes yet.
mesh-roaming-empty = No settings are roaming to this device yet.
mesh-roaming-none = No personal settings or saved Wi-Fi networks are roaming.
mesh-home-caption = Choose Pair home server while your new Tilo server is starting. It receives its identity and full-sync setup automatically.
mesh-privacy-title = Share with your devices
mesh-roaming-title = Roaming settings
mesh-activity-title = Recent activity
mesh-trusted-title = Your devices
mesh-home-nas = Home server or NAS
mesh-checking = Checking…
mesh-name = Name
mesh-availability = Availability
mesh-files-stay-desc = Revoking stops this server receiving new changes. Make sure another device has any files whose only full copy is here.
mesh-svc-relay = Collaboration and chat relay
mesh-svc-relay-desc = Keep workspace updates and messages moving between devices
mesh-svc-remote = Remote-control relay
mesh-svc-remote-desc = Connect remote-control sessions through this device when needed
mesh-svc-apps = Serve app backends
mesh-svc-apps-desc = Run compatible app services from this device
mesh-svc-jobs = Accept distributed jobs
mesh-svc-jobs-desc = Run compatible background jobs from this device
mesh-status-shown-not-saved = Not saved: { $error }
mesh-status-shown-home-paired = Home server paired
mesh-status-shown-device-added = Personal device added
mesh-fingerprint-label = Identity fingerprint
mesh-remove-device = Remove device…
mesh-remove-home = Remove home server…
mesh-remove-device-confirm = Remove device
mesh-remove-home-confirm = Remove home server
mesh-cancel = Cancel
mesh-remove-ask = Remove { $label }?
continuity-clipboard = Clipboard
continuity-notifications = Notifications
continuity-wifi-settings = Wi-Fi and settings
continuity-media = Media
continuity-display-input = Display & input
continuity-file-transfer = File transfer
continuity-realtime-sessions = Live sessions
continuity-typed-gamepad = Game controllers
continuity-app-continuations = App handoff
continuity-all = All sharing
continuity-audit-allowed = { $subject } allowed
continuity-audit-stopped = { $subject } stopped
settings-appearance-theme-card = Theme and size
settings-appearance-mode = Appearance
settings-appearance-mode-light = Light
settings-appearance-density = Density
settings-appearance-density-compact = Compact
settings-appearance-density-regular = Regular
settings-appearance-density-roomy = Roomy
settings-appearance-ui-scale = Interface size
settings-appearance-accessibility = Accessibility options
settings-appearance-accessibility-desc = High contrast, reduce motion and text size
settings-appearance-accessibility-open = Open Accessibility
settings-appearance-reduce-transparency-desc = Make menus, panels and sheets opaque
settings-appearance-distinct-letters-desc = Set I, l and 1, and 0 and O apart in all text, not only in names, codes and links
settings-appearance-text-small = Small
settings-appearance-text-default = Default
settings-appearance-text-large = Large
settings-appearance-text-larger = Larger
settings-appearance-wallpaper-card = Wallpaper
settings-appearance-use-for = Use for
settings-appearance-show-on = Show on
settings-appearance-scope-workspace = This workspace
settings-appearance-scope-all = Every workspace
settings-appearance-target-desktop = Desktop
settings-appearance-target-lock = Lock screen
settings-appearance-target-both = Desktop and lock screen
settings-appearance-your-picture = Your picture
settings-wallpaper-alps = Alpine haze
settings-wallpaper-cloud-sea = Sea of clouds
settings-wallpaper-dunes = Desert dunes
settings-wallpaper-frost-tree = Tree in fog
settings-wallpaper-rainier = Mountain alpenglow
settings-wallpaper-moonlit-fjord = Moonlit fjord
settings-wallpaper-earthset = Earthset
settings-wallpaper-andromeda = Andromeda galaxy
settings-appearance-choose-wallpaper = Choose a wallpaper
settings-appearance-choose = Choose
settings-appearance-pictures = Pictures
settings-appearance-confirm-preview = Confirm the preview to use it.
settings-appearance-open-failed = Could not open the picture: { $error }
settings-appearance-updated = Wallpaper updated.
settings-appearance-unchanged = The wallpaper was not changed: { $error }
settings-appearance-schedule = Schedule
settings-appearance-schedule-sun = Sunset to sunrise
settings-appearance-schedule-custom = Custom times
settings-appearance-day-starts = Day starts
settings-appearance-night-starts = Night starts
settings-appearance-today = Today
settings-appearance-today-value = Day from { $day }, night from { $night }
settings-appearance-schedule-no-location = Your time zone gives no location, so day runs from the custom times. The Automatic theme and a scheduled night light follow this schedule.
settings-appearance-day-night-card = Day and night
a11y-vision-title = Vision
a11y-high-contrast = High contrast
a11y-high-contrast-desc = Maximise contrast for low-vision comfort
a11y-reduce-motion = Reduce motion
a11y-reduce-motion-desc = Skip animations and transitions
a11y-colour-filter = Colour filter
a11y-magnifier = Magnifier
a11y-magnifier-view = Magnifier view
a11y-pointer-title = Pointer
a11y-mouse-keys = Mouse keys
a11y-mouse-keys-desc = Move the pointer with the numeric keypad
a11y-large-cursor = Large cursor
a11y-large-cursor-desc = Enlarge the mouse pointer so it is easy to find
a11y-cursor-size = Cursor size
a11y-pointer-crosshair = Pointer crosshair
a11y-pointer-crosshair-desc = Draw high-contrast guide lines through the pointer
a11y-thick-caret = Thicker text cursor
a11y-thick-caret-desc = Use a wider insertion caret in Tilo applications
a11y-typing-title = Typing
a11y-osk = On-screen keyboard
a11y-osk-desc = Automatic shows it for text fields while you use touch without typing on a keyboard
a11y-sticky-keys = Sticky keys
a11y-sticky-keys-desc = Type shortcuts one key at a time instead of holding a chord
a11y-slow-keys = Slow keys
a11y-slow-keys-desc = Ignore brief, accidental key presses
a11y-bounce-keys = Bounce keys
a11y-bounce-keys-desc = Ignore a key pressed again right after it was released
a11y-bounce-delay = Bounce keys delay
a11y-bounce-alert = Flash when a key is ignored
a11y-bounce-alert-desc = Show the visual alert each time bounce keys drops a press
a11y-switch-control = Switch control
a11y-switch-control-desc = Translate a hardware switch pair into standard Tab and Enter actions
a11y-switch-keys = Switch keys (next and activate)
a11y-switch-scan = Automatic scan
a11y-advanced-title = More assistive technology
a11y-screen-reader = Screen reader
a11y-screen-reader-desc = Read focused controls and live announcements aloud
a11y-speech-engine = Speech engine
a11y-speech-rate = Speech rate
a11y-voice = Voice
a11y-screen-reader-missing = Not available on this device
a11y-visual-alerts = Visual alerts
a11y-visual-alerts-desc = Flash a clear screen-edge alert when a notification arrives
a11y-focus-highlight = Application focus highlight
a11y-focus-highlight-status = Not yet available for non-Tilo applications
a11y-live-captions = Live captions
a11y-live-captions-status = Not available on this device
settings-about-backup-card = Identity backup
settings-about-backup-button = Create encrypted backup…
settings-about-system-card = System
settings-about-kernel = Kernel
settings-about-architecture = Architecture
settings-about-uptime = Uptime
settings-about-hardware-card = Hardware
settings-about-processor = Processor
settings-about-memory = Memory
accounts-linked-title = Linked accounts
accounts-linked-about = Your accounts on the services this room is connected to. Linking one lets the room know your messages there are yours.
accounts-linked-link = Link { $service } account
accounts-linked-copy = Copy
accounts-linked-unlink = Unlink
accounts-browser-sessions-title = Website accounts
accounts-browser-sessions-about = Forget a selected account on this device. The browser closes its website session and removes its saved sign-in and local profile; other accounts are unchanged.
accounts-browser-sessions-remove = Forget account on this device
accounts-browser-sessions-removing = Waiting for the browser to forget this account…
accounts-browser-identity-title = Tilo browser identities
accounts-browser-identity-about = Public account choices for existing browser sign-ins. Hiding or removing a choice does not revoke sign-in or resource permissions.
accounts-browser-identity-hide = Hide from browser picker
accounts-browser-identity-show = Show in browser picker
accounts-browser-identity-remove = Remove browser choice

accounts-vault-local = No keyring is available, so saved credentials are kept in a private file on this device. Unlock or install a keyring for stronger protection.
accounts-empty-sample = You have no accounts yet. The entries below are examples. Choose a provider under “Add account” to connect your own.
accounts-empty = You have no accounts yet. Choose a provider under “Add account” to connect your first.
accounts-remove-title = Remove account?
accounts-remove-body = { $name } will be removed from this device. Locally cached copies may be removed by its apps; data on the provider is not deleted. The encrypted credential and local sync-health record are removed.
accounts-cancel = Cancel
accounts-add = Add account
accounts-add-title = Add an account
accounts-best-effort-icloud = Best effort: iCloud Drive has no public API, so Tilo uses the web API iCloud.com uses. It may stop working until Tilo is updated.
accounts-status-example = Example
accounts-status-disabled = Disabled
accounts-status-syncing = Syncing
accounts-status-healthy = Up to date
accounts-status-offline = Offline
accounts-status-reauth = Sign in again
accounts-status-error = Needs attention
accounts-status-never = Not checked yet
accounts-best-effort = Best effort
accounts-sync-never = never completed
accounts-sync-when = Last successful sync: { $when }
accounts-sync-when-msg = Last successful sync { $when }: { $message }
accounts-reauth = Sign in again
accounts-reauth-icloud = Re-authenticate
accounts-edit = Edit
accounts-check-server = Check server
accounts-verify = Verify sign-in
accounts-remove = Remove
accounts-status-paused = Sync paused
accounts-sync-now = Sync now
accounts-pause-sync = Pause sync
accounts-resume-sync = Resume sync
accounts-cloud-unavailable = The cloud account service is not running
accounts-display-name = Display name
accounts-email = Email address
accounts-username = Username
accounts-username-or-email = Username or email
accounts-login-name = Login name
accounts-login-placeholder = Username (defaults to your address)
accounts-password = Password or app password
accounts-password-short = Password
accounts-server = Server
accounts-server-url = Server URL
accounts-server-mail = Detected from your email address
accounts-save = Save account
accounts-form-add = Add { $kind } account
accounts-form-edit = Edit { $kind } account
accounts-save-error = Could not save: { $error }
accounts-reauth-password = New password or app password
accounts-reauth-account = Account
accounts-reauth-credential = Credential
accounts-reauth-error = Could not update credential: { $error }
accounts-reauth-save = Update credential
accounts-reauth-title = Replace saved password
accounts-checking = Checking the account before saving…
accounts-checking-reauth = Checking the replacement credential…
accounts-gone = account no longer exists
accounts-timeout = connection check timed out
accounts-kind-mail = Email
accounts-kind-caldav = Calendar
accounts-kind-webdav = Files (WebDAV)
accounts-kind-nextcloud = Nextcloud
accounts-kind-google = Google
accounts-google-files-service = Google Drive files
accounts-google-mail-service = Gmail
accounts-google-empty-services = Connect Google for Files, Mail, or both. Gmail-only accounts appear under Email.
accounts-google-mail-consent = Mail allows Tilo to read, send and delete Gmail messages. If Google denies access or the mailbox cannot be validated, nothing is saved.
accounts-service-files = Files
accounts-service-mail = Mail
accounts-services-continue = Continue
accounts-google-provider = Google
accounts-form-cancel = Cancel
accounts-files-mode = Files
accounts-files-sync = Keep files in sync
accounts-files-import = Import once
accounts-files-mode-description = Sync keeps files connected to the cloud. Import copies files once into Tilo without uploading changes back to the provider.
accounts-kind-onedrive = OneDrive
accounts-kind-dropbox = Dropbox
accounts-kind-icloud = iCloud Drive
agent-add-vertex = Add Google Vertex AI
agent-add-gemini = Add Gemini with Google
agent-gemini-title = Google Gemini
agent-gemini-intro = Use Gemini models billed to your own Google Cloud project. Sign in with Google in your browser: Tilo asks only for access to Google Cloud, separate from Mail and Files, and needs no API key.
agent-gemini-choose = Signed in. Choose the Google Cloud project to bill.
agent-gemini-requirements = The Gemini API (Generative Language API) must be enabled in this project. Requests are billed to the project.
agent-sign-in-title = Sign in to a provider
agent-sign-in-intro = Use a subscription you already have, or your own Google Cloud project, without an API key. The sign-in stays on this device; only the model service uses it, never an agent.
agent-sign-in-chatgpt = Sign in with ChatGPT
agent-sign-in-copilot = Sign in with GitHub Copilot
agent-sign-in-openrouter = Sign in with OpenRouter
agent-sign-in-huggingface = Sign in with Hugging Face
agent-sign-in-chatgpt-unofficial = OpenAI doesn't offer ChatGPT sign-in to other apps. It works the way OpenAI's own Codex signs in, and may stop working when OpenAI changes it. An OpenAI API key keeps working.
agent-sign-in-waiting = Continue in your browser. This page updates when you're done.
agent-sign-in-device-asking = Getting a sign-in code from GitHub.
agent-sign-in-device-code = Enter the code { $code } at { $page }. This page updates when you're done.
agent-sign-in-cancelled = Sign-in cancelled. Nothing was saved.
agent-sign-in-stopped = The sign-in stopped unexpectedly. Nothing was saved.
agent-sign-in-ports-busy = Another program is using the port this sign-in needs. Close it and try again.
agent-sign-in-browser-failed = Couldn't open your browser: { $error }. Nothing was saved.
agent-sign-in-not-configured = Signing in to { $service } isn't set up in this build. Use an API key instead.
agent-sign-in-offline = Can't reach { $service }. Check your connection and try again.
agent-sign-in-failed = Couldn't sign in to { $service }: { $error }
agent-sign-in-done = Signed in to { $service }.
agent-sign-in-unsupported = A sign-in this version doesn't know
agent-vertex-title = Google Vertex AI
agent-vertex-intro = Use Gemini models billed to your own Google Cloud project. Sign in with Google in your browser: Tilo asks only for access to Google Cloud, separate from Mail and Files, and needs no API key.
agent-vertex-sign-in = Sign in with Google
agent-vertex-waiting = Continue in your browser: choose your Google account and allow access to Google Cloud. This page updates when you are done.
agent-vertex-choose = Signed in. Choose the Google Cloud project to bill, where requests run, and the model.
agent-vertex-projects-unlisted = Your projects could not be listed ({ $error }). Type the project ID shown in the Google Cloud console.
agent-vertex-projects-none = No projects were found for this account. Type the project ID shown in the Google Cloud console.
agent-vertex-project-named = { $name } ({ $id })
agent-vertex-project = Google Cloud project
agent-vertex-project-hint = Project ID, for example my-project-123
agent-vertex-location = Location
agent-vertex-location-hint = global, or a region such as europe-west4
agent-vertex-model-hint = gemini-4-argon, or another model ID
agent-vertex-requirements = The Vertex AI API must be enabled in this project, and your Google account needs permission to use it. Requests are billed to the project.
agent-vertex-add-provider = Add provider
agent-vertex-not-configured = Google sign-in for Vertex AI is not configured in this build.
agent-vertex-ports-busy = Another program is using the ports Tilo listens on for Google's answer. Close it and try again.
agent-vertex-browser-failed = Could not open your browser: { $error }. Nothing was saved.
agent-vertex-sign-in-failed = Could not sign in with Google: { $error }
agent-vertex-scope-missing = Google Cloud access was not allowed. Sign in again and allow it.
agent-vertex-stopped = The sign-in stopped unexpectedly.
agent-vertex-sign-in-first = Sign in with Google first.
agent-vertex-signed-in = Signed in with Google again.
agent-vertex-project-invalid = Enter a Google Cloud project ID: lowercase letters, digits and hyphens.
agent-vertex-location-invalid = Enter a Vertex AI location such as global or europe-west4.
agent-vertex-model-invalid = A model ID has no spaces.
agent-signed-in = Signed in with { $service } on this device.
agent-sign-in-again = Sign in again
agent-sign-in-again-needed = Sign in again
agent-sign-in-unknown = Sign-in status unknown.
agent-sign-in-read-error = Can't tell whether the Google sign-in still holds: { $error }
agent-sign-in-forget-err = The Google sign-in was not removed from this device: { $error }
sound-output = Output
sound-input = Input
sound-volume = Volume
sound-mute = Mute
sound-unmute = Unmute
sound-app-volume = { $app } volume
sound-balance = Balance
sound-balance-centred = Centred
sound-balance-left-by = { $amount } left
sound-balance-right-by = { $amount } right
sound-mono = Mono audio
sound-test-left = Left
sound-test-right = Right
sound-test-caption = Plays a short, quiet tone only on the selected channel. Tests stop automatically.
sound-test-row = Speaker test
sound-input-level = Input level
sound-mic-idle = Microphone test records no audio
sound-mic-listening = Speak to see the level
sound-mic-stop = Stop test
sound-mic-test = Test microphone
sound-echo = Reduce echo in calls
sound-hardware = Device capabilities
sound-profile = Device profile
sound-choose-profile = Choose a profile
sound-codec = { $device } codec
sound-apps = Applications
sound-apps-empty = No applications are playing audio.
sound-recording = Recording
sound-recording-empty = No applications are recording.
sound-follow-default = Default
sound-speaker-set-detail = { $count ->
    [0] Speaker set
    [one] Speaker set, 1 speaker
   *[other] Speaker set, { $count } speakers
}
sound-speakers-title = Speakers across your devices
sound-speakers-row = Stereo pair or room speakers
sound-speakers-desc = Play through speakers on several of your devices at once
sound-speakers-open = Set up speakers…
sound-system = System sounds
sound-system-missing = This device can’t play system sounds.
sound-theme = Sound theme
sound-event-notification = Notifications
sound-event-message = New messages
sound-event-volume = Volume change feedback
sound-event-screenshot = Screenshot shutter
sound-event-connected = Device connected
sound-event-disconnected = Device disconnected
sound-event-battery = Low battery
sound-event-error = Errors
sound-notice-stream-route = Audio stream route changed.
sound-notice-stream-route-err = Audio stream route could not be changed: { $error }
sound-notice-stream-vol = Audio stream volume changed.
sound-notice-stream-vol-err = Audio stream volume could not be changed: { $error }
sound-notice-stream-mute = Audio stream mute changed.
sound-notice-stream-mute-err = Audio stream mute could not be changed: { $error }
sound-notice-pref-err = Audio preference could not be saved: { $error }
sound-notice-test-done = Speaker test finished.
sound-notice-mic-done = Microphone test finished; no audio was saved.
sound-notice-out-vol = Output volume set to { $percent }%.
sound-notice-out-vol-err = Output volume could not be changed: { $error }
sound-notice-in-vol = Input level set to { $percent }%.
sound-notice-in-vol-err = Input level could not be changed: { $error }
sound-notice-muted = Output muted.
sound-notice-unmuted = Output unmuted.
sound-notice-mute-err = Output mute could not be changed: { $error }
sound-notice-mono-on = Mono output enabled.
sound-notice-mono-off = Mono output disabled.
sound-notice-mono-active = Mono output is active for this desktop session.
sound-notice-mono-restored = Mono output is off; the physical output is restored.
sound-notice-sounds-ok = System sounds updated.
sound-notice-echo-on = Calls use the echo-cancelled microphone.
sound-notice-echo-off = Calls use the microphone directly.
sound-notice-test-need = The speaker test needs stereo output.
sound-notice-playing = Playing a short { $channel }-channel tone…
sound-notice-test-err = Could not test the { $channel } speaker: { $error }
sound-notice-mic-stopped = Microphone test stopped; no audio was saved.
sound-notice-mic-none = No microphone is connected.
sound-notice-mic-started = Microphone level test started for eight seconds.
sound-notice-mic-err = Could not test the microphone: { $error }
sound-notice-device-err = Output device could not be changed: { $error }

sound-notice-out-device = Audio output device changed.
sound-notice-in-device = Audio input device changed.
sound-notice-in-device-err = Input device could not be changed: { $error }
sound-notice-profile = Audio device profile changed.
sound-notice-profile-err = Audio device profile could not be changed: { $error }
sound-notice-test-started = { $channel } speaker test started.
settings-bluetooth-card = Bluetooth
settings-bluetooth-cancel-pairing = Cancel pairing
settings-bluetooth-pair = Pair
settings-bluetooth-disconnect = Disconnect
settings-bluetooth-connect = Connect
settings-bluetooth-connected = Connected
settings-bluetooth-paired = Paired
settings-bluetooth-nearby = Nearby
settings-bluetooth-status-battery = { $status }, battery { $level }%
settings-bluetooth-address = Address
settings-bluetooth-signal = Signal
settings-bluetooth-signal-dbm = { $rssi } dBm
settings-bluetooth-trust = Trust this device
settings-bluetooth-forget = Forget…
settings-bluetooth-looking = Looking for nearby devices…
settings-bluetooth-empty = No devices found. Put a device in pairing mode, then look for devices.
settings-bluetooth-stop = Stop looking
settings-bluetooth-find = Look for devices
settings-bluetooth-connected-name = Connected { $name }.
settings-bluetooth-disconnected-name = Disconnected { $name }.
settings-bluetooth-connect-failed = Could not connect { $name }: { $error } Check that it is awake and in range, then retry.
settings-bluetooth-disconnect-failed = Could not disconnect { $name }: { $error } Check that it is awake and in range, then retry.
settings-bluetooth-paired-connected = { $name } is paired and connected.
settings-bluetooth-paired-not-connected = { $name } paired, but did not connect: { $reason } Use Connect to retry.
settings-bluetooth-pair-failed = Could not pair { $name }: { $error } Check that it is awake and in pairing mode, then retry.
settings-bluetooth-on = Bluetooth is on.
settings-bluetooth-off = Bluetooth is off.
settings-bluetooth-power-failed = Could not change Bluetooth power: { $error }
settings-bluetooth-discovery-started-notice = Looking for devices nearby.
settings-bluetooth-discovery-stopped-notice = Discovery stopped.
settings-bluetooth-discovery-failed = Could not change discovery: { $error }
settings-bluetooth-refreshed = Bluetooth devices refreshed.
settings-bluetooth-pairing = Pairing { $name }… Security prompts appear in system UI.
settings-bluetooth-pairing-busy = Another device is pairing; wait for it to finish.
settings-bluetooth-device-gone = That device is no longer known to this computer.
settings-bluetooth-cancelled = Cancelled pairing { $name }.
settings-bluetooth-cancel-failed = Could not cancel pairing: { $error }
settings-bluetooth-now-trusted = { $name } is now trusted.
settings-bluetooth-now-untrusted = { $name } now requires service authorisation.
settings-bluetooth-trust-failed = Could not change device trust: { $error }
settings-bluetooth-renamed = Renamed device to { $alias }.
settings-bluetooth-rename-failed = Could not rename { $name }: { $error }
settings-bluetooth-forgot = Forgot { $name }.
settings-bluetooth-forget-failed = Could not forget { $name }: { $error }
input-keyboard-title = Typing
input-keyboard-caption = Choose how quickly held keys repeat.
input-active-source = Active input source
input-key-repeat = Key repeat speed
input-key-repeat-off = Repeat is off. Move the slider to turn it on.
input-key-repeat-rate = { $rate } keys per second, starts after { $delay } ms
input-key-repeat-faster = Faster repeat starts sooner
input-compose-key = Compose key
input-compose-disabled = Disabled
input-compose-ralt = Right Alt
input-compose-menu = Menu
input-compose-caps = Caps Lock
input-compose-rwin = Right Super
input-compose-lwin = Left Super
input-caps-lock-key = Caps Lock key
input-caps-lock-caps = Caps Lock
input-caps-lock-ctrl = Control
input-caps-lock-esc = Escape
input-caps-lock-backspace = Backspace
input-spell-title = Spell checker
input-spell-check = Check spelling while typing
input-spell-check-desc = Underline possible misspellings in natural-language fields
input-spell-auto = Auto-correct obvious misspellings
input-spell-auto-desc = Apply only high-confidence corrections after Space, punctuation, or Enter
input-spell-language = Spelling language
input-spell-language-session = Session language
input-spell-caption = Press Backspace immediately to restore the original word. Passwords are always excluded; other structured fields require an app to opt in.
input-sources-title = Input sources
input-sources-active = Active
input-sources-remove = Remove
input-sources-add = Add a language, layout or input method
input-sources-choose = Choose a source
input-sources-full = Up to four layouts can be active at once.
input-sources-ime = Input methods (Rime, Anthy, Hangul) compose text on your first keyboard layout. While one is active, the on-screen keyboard steps aside.
input-sources-move-up = Move input source up
input-sources-move-down = Move input source down
input-accel-adaptive = Adaptive
input-accel-flat = Flat
net-pending-title = Network
net-pending = Reading network status from NetworkManager…
net-vpn-add = Add a VPN
net-now-portal = Sign in to use this network
net-connected-no-internet = Connected, no internet access
net-now-airplane = Airplane mode is on
net-wifi-hardware-switch = Wi-Fi is off with the hardware switch
net-scanning = Looking for networks…
net-none-in-range = No networks in range
net-enterprise-entry = Connect to a work or school network…
net-enterprise-entry-detail = Networks that sign in with a username
net-autoconnect = Join automatically
net-metered-connection = Metered connection
net-metered-caption = Updates and backups wait for another network
net-share = Share
net-forget-network = Forget this network
net-diag-check = Connection check
net-diag-check-caption = Address, gateway and name servers
net-connectivity-unknown = Connectivity not checked
net-connectivity-offline = Offline
net-connectivity-portal = Sign-in required
net-connectivity-limited = Limited connectivity
net-connectivity-full = Connected to the internet
net-open-signin = Open sign-in page
net-status = Status
net-wifi-off = Wi-Fi is off
net-connected-to = Connected to { $ssid }
net-not-connected = Not connected
net-no-wifi-adapter = No Wi-Fi adapter on this device
net-no-modem = No mobile broadband modem on this device
net-no-radio = No Wi-Fi or mobile broadband radio on this device
net-applying = Applying network changes…
net-refresh = Refresh
net-wifi = Wi-Fi
net-wired = Wired
net-airplane = Airplane mode
net-airplane-caption = Disables Wi-Fi and mobile broadband together.
net-hotspot-active = Active
net-hotspot-off = Off
net-hotspot-name = Hotspot name
net-hotspot-password = Password (at least 8 characters)
net-band = Band
net-band-auto = Automatic
net-band-24 = 2.4 GHz
net-band-5 = 5 GHz
net-apply-start = Apply and start
net-hide-qr = Hide QR
net-share-qr = Share QR
net-qr-caption = Scan to join. The QR code contains the hotspot password.
net-hotspot-clients = Connected devices
net-signal-unknown = signal unknown
net-signal-weak = weak signal
net-signal-fair = fair signal
net-signal-good = good signal
net-signal-strong = strong signal
net-security-secured = Secured
net-security-open = Open
net-hotspot = Wi-Fi hotspot
net-share-devices = Share with my devices
net-forget = Forget
net-edit-profile = Edit profile
net-metered-auto = Automatic
net-metered-yes = Metered
net-metered-no = Unmetered
net-mac-stable = Stable per network
net-mac-random = Random each connection
net-mac-permanent = Permanent hardware address
net-run-diagnostics = Run diagnostics
net-diag-title = Connection diagnostics
net-notice-sealed-profile = Shared { $ssid } with your other devices.
net-diag-internet = Internet
net-diag-device = Device
net-diag-address = Address
net-diag-gateway = Gateway
net-diag-dns = DNS
net-diag-dns-none = Not provided
net-vpn = VPN
net-mobile = Mobile
net-profile-name = Profile name
net-apn = Access point name (APN)
net-sim = SIM
net-unlock-sim = Unlock SIM
net-unlock-sim-ask = Unlock SIM…
net-mobile-data = Mobile data
net-mobile-roaming = roaming
net-mobile-connected = connected
net-mobile-registered = Registered
net-mobile-searching = Looking for a network
net-mobile-denied = The network refused this SIM
net-mobile-emergency-only = Emergency calls only
net-mobile-not-registered = Not on a network
net-mobile-data-off = Mobile data is off
net-mobile-no-sim-card = No SIM card
net-mobile-sim-error = The SIM card can't be read
net-mobile-esim-empty = The eSIM has no profile
net-mobile-modem-failed = The modem isn't working
net-mobile-sim-locked = SIM locked
net-mobile-sim-blocked = SIM blocked
net-mobile-carrier-locked = Locked to another operator
net-mobile-carrier-locked-explain = This modem accepts only SIMs of the operator it is locked to. The operator can unlock it.
net-mobile-pin-explain = Enter the SIM's PIN to use mobile data.
net-mobile-puk-explain = The SIM is blocked after too many wrong PINs. Enter the PUK from the SIM card's package and choose a new PIN.
net-mobile-tries-left = { $count ->
    [one] One try left.
   *[other] { $count } tries left.
}
net-mobile-new-pin = New PIN
net-mobile-current-pin = Current PIN
net-mobile-unblock = Unblock SIM
net-mobile-unblock-ask = Unblock SIM…
net-mobile-checking = Checking the code…
net-mobile-wrong-pin = Wrong PIN
net-mobile-wrong-puk = Wrong PUK
net-mobile-unlocked = SIM unlocked.
net-mobile-pin-changed = PIN changed.
net-mobile-not-locked = No SIM asks for its PIN
net-mobile-not-blocked = No SIM is blocked
net-mobile-no-sim = No SIM is in use
net-mobile-no-such-sim = No SIM { $sim }
net-mobile-single-slot = This SIM has no slot to switch to
net-mobile-read-err = Can't read the modem: { $error }
net-mobile-profile-err = Can't read the mobile data connection: { $error }
net-mobile-change-err = The modem refused: { $error }
net-mobile-unknown-choice = No choice called { $choice }
net-mobile-roaming-setting = Data roaming
net-mobile-roaming-caption = Use mobile data on other operators' networks, which can cost extra.
net-mobile-metered = Metered connection
net-mobile-metered-caption = Apps hold back large downloads on a metered connection.
net-mobile-metered-automatic = Automatic
net-mobile-metered-yes = Metered
net-mobile-metered-no = Not metered
net-mobile-network-type = Preferred network type
net-mobile-network-any = Any
net-mobile-network-only = { $network } only
net-mobile-network-range = { $slowest } to { $fastest }
net-mobile-network-preferred = { $networks }, { $preferred } preferred
net-mobile-apn-automatic = Automatic
net-mobile-apn-edit = Edit
net-mobile-apn-auto = Choose automatically
net-mobile-apn-auto-caption = Use the access point the operator publishes for this SIM.
net-mobile-apn-placeholder = internet
net-mobile-apn-empty = Enter an access point name, or choose it automatically.
net-mobile-save = Save
net-mobile-cancel = Cancel
net-mobile-pin-lock-turn-on = Ask for PIN
net-mobile-pin-lock-turn-off = Stop asking for PIN
net-mobile-data-used = Data used
net-mobile-used = { $used } since { $month } { $day }
net-mobile-used-reset = Reset
net-mobile-reset-day = Count starts again on day
net-mobile-reset-day-range = The day is between 1 and 28.
net-mobile-sim-title = SIM
net-mobile-sim-card = SIM card
net-mobile-esim = eSIM
net-mobile-esim-no-profile = eSIM, no profile
net-mobile-slot = slot { $slot }
net-mobile-in-use = In use
net-mobile-use-sim = Use
net-mobile-number = Phone number
net-mobile-pin-lock = Ask for PIN at start-up
net-mobile-pin-lock-caption = Without the PIN, nobody can use this SIM in another device.
net-mobile-pin-lock-on-explain = Enter the SIM's PIN to start asking for it.
net-mobile-pin-lock-off-explain = Enter the SIM's PIN to stop asking for it.
net-mobile-change-pin-open = Change PIN…
net-mobile-change-pin = Change PIN
net-mobile-change-pin-explain = Enter the current PIN and a new one of 4 to 8 digits.
net-method = Method
net-inner-auth = Inner authentication
net-client-cert = Client certificate path (TLS)
net-private-key = Private key path (TLS)
net-ssid = Network name (SSID)
net-identity = Username or identity
net-anonymous = Anonymous identity (optional)
net-ca-path = Absolute path to CA certificate
net-choose-ca = Choose CA certificate…
net-password = Password
net-save-connect = Save and connect
net-connected = Connected
net-connecting = Connecting…
net-join = Join
net-ethernet = Ethernet
net-wired-empty = Plug in an Ethernet cable to use a wired connection.
net-connected-speed = Connected, { $speed }
net-interface = Interface
net-ipv4 = IPv4
net-ipv4-unsupported = This profile's IPv4 method is unsupported or could not be read; Settings will leave it unchanged.
net-auto-ipv4 = Automatic IPv4
net-address-prefix = Address/prefix (for example 192.0.2.10/24)
net-gateway = Gateway
net-dns-servers = DNS servers
net-apply-ip = Apply IP settings
net-notice-unavailable = Network status is unavailable while the window closes.
net-notice-refreshed = Network list refreshed.
net-notice-refresh-err = Could not refresh network list: { $error }
net-notice-wifi-on = Wi-Fi enabled.
net-notice-wifi-off = Wi-Fi disabled.
net-notice-wifi-err = Could not change Wi-Fi power: { $error }
net-notice-wired-on = Wired networking enabled.
net-notice-wired-off = Wired networking disabled.
net-notice-wired-err = Could not change wired networking: { $error }
net-notice-radio-on = { $radio } enabled.
net-notice-radio-off = { $radio } disabled.
net-notice-radio-err = Could not change { $radio } state: { $error }
net-radio-airplane = airplane
net-radio-hotspot = hotspot
net-notice-update-err = Could not update { $name }: { $error }
net-notice-forgot = Forgot { $name }.
net-notice-forget-err = Could not forget { $name }: { $error }
net-notice-hotspot-on = Hotspot is active.
net-notice-hotspot-err = Hotspot failed: { $error }
net-notice-ip-ok = Applied IP settings to { $name }.
net-notice-ip-err = IP settings were not applied: { $error }
net-notice-policy-ok = Updated { $name }.
net-notice-policy-err = Policy was not applied: { $error }
net-notice-policy-unread = Policy was not applied because NetworkManager's current values could not be read.
net-notice-connecting = Connecting to { $ssid }…
net-notice-used-sealed = Used the sealed password from your certified devices.
net-notice-password-sealed = Password sealed for your certified devices.
net-notice-share-err = Password could not be shared: { $error }
net-notice-share-invalid = Password could not be shared: the continuity service returned an invalid reply.
net-notice-cert-err = Could not choose certificate: { $error }
net-notice-connected = Connected to { $subject }.
net-notice-connected-extra = Connected to { $subject }. { $notice }
net-notice-enter-password = Enter the password for { $ssid }.
net-notice-enter-password-warn = Enter the password for { $ssid }. Shared password unavailable: { $warning }
net-notice-configure-err = Could not configure { $subject }: { $error }
net-notice-hotspot-starting = Starting the hotspot…
net-subject-saved-wifi = saved Wi-Fi profile
net-subject-enterprise = enterprise Wi-Fi

## Network pages
net-show-all = Show all networks ({ $count })
net-all-networks = All networks
net-in-range-count = In range ({ $count })
net-none-match = No network in range matches the search.
net-hidden-entry = Other network…
net-hidden-title = Other network
net-hidden-caption = Join a network that does not broadcast its name.
net-hidden-needs-name = Enter the network's name.
net-password-optional = Password (none for an open network)
net-known = Known networks
net-known-count = Known networks ({ $count })
net-known-empty = This device has not joined a Wi-Fi network yet.
net-known-short = Known
net-last-connected = Last connected { $when }
net-never-connected = Never connected
net-out-of-range = Not in range
net-in-range = In range
net-more-for = More for { $name }
net-network-settings = Network settings
net-network-settings-of = Settings of { $ssid }
net-advanced = Advanced
net-enterprise-title = Work or school networks
net-settings = Settings
net-addresses = Addresses
net-actions = Actions
net-gone = This network is no longer in range or saved.
net-security = Security
net-security-type-open = Open
net-security-type-owe = Enhanced open (OWE)
net-security-type-wpa2 = WPA2 Personal
net-security-type-wpa3 = WPA3 Personal
net-security-type-enterprise = WPA Enterprise (802.1X)
net-security-type-other = Other
net-band-6 = 6 GHz
net-link-speed = Link speed
net-ipv6 = IPv6
net-dns = DNS
net-hardware-address = Hardware address
net-mac-caption = A random address keeps networks from following this device.
net-priority = Priority
net-priority-caption = Among known networks in range, the higher is joined first.
net-hidden-network = Hidden network
net-hidden-network-caption = Look for this network even when it does not broadcast its name.
net-show-password = Show password
net-hide-password = Hide password
net-password-hidden = Hidden
net-new-password = New password
net-change-password = Change password
net-password-needed = Enter the new password.
net-apply-identity = Apply identity
net-dns-caption = Comma-separated; empty takes the network's.
net-dns-automatic = Automatic
net-ipv6-method = IPv6
net-ipv6-auto = Automatic
net-ipv6-manual = Manual
net-ipv6-link-local = Link-local only
net-ipv6-off = Off
net-ipv6-addresses = IPv6 addresses with prefix
net-apply-ipv6 = Apply IPv6 settings
net-proxy = Proxy
net-proxy-none = None
net-proxy-auto = Automatic (configuration URL)
net-proxy-manual = Manual
net-proxy-address = Configuration URL or host:port
net-proxy-caption = Automatic takes a configuration URL; Manual a proxy's host:port.
net-proxy-needs-address = Enter the proxy's configuration URL or host:port.
net-apply-proxy = Apply proxy
net-share-qr-title = Share by QR code
net-share-qr-caption = A phone that scans the code joins this network.
net-qr-network-caption = The code holds the network's password.
net-share-needs-saved = Join the network on this device first to share it.
net-choose = Choose
net-choose-ca-title = Choose organization CA certificate
net-notice-applying = Applying settings to { $name }…
net-notice-sealing = Sharing { $name } with your devices…
net-notice-diagnosed = Network diagnostics completed.
net-notice-diagnose-err = Diagnostics unavailable: { $error }
net-notice-signin-err = Could not open network sign-in: { $error }
net-notice-password-err = Could not read the password: { $error }
net-notice-cert-chosen = CA certificate selected.
net-notice-enterprise-connecting = Connecting to the work or school network…
net-notice-enterprise-connected = Enterprise Wi-Fi connected.

## VPN
net-device-vpn-through = Through { $device }
net-device-vpn-unnamed = your other device
net-device-vpn-reconnecting = Reconnecting…
net-device-vpn-blocking = Reconnecting. Nothing goes out until it is back.
net-device-vpn-not-yours = It does not count this device as yours.
net-device-vpn-exit-off = It is not shared as a VPN right now.
net-device-vpn-full = It has no room for another device right now.
net-device-vpn-not-installed = The part that routes traffic is not installed here.
net-device-vpn-setup = The route could not be set up.
net-device-vpn-no-home = Its home network is out of reach from here.
net-device-vpn-exit = This device as a VPN
net-device-vpn-share = Let my other devices use this device as a VPN
net-device-vpn-share-caption = Their traffic goes out through this device's connection.
net-device-vpn-kill-switch = Block traffic while the tunnel is down
net-device-vpn-kill-switch-caption = Nothing goes out directly while the device it goes through cannot be reached.
net-device-vpn-no-route = This device goes through none of your devices.
net-device-vpn-using = Using this device as a VPN
net-device-vpn-stop = Stop
net-device-vpn-stopped = { $device } no longer uses this device as a VPN.
net-device-vpn-unavailable = Your devices as a VPN are not available here.
net-device-vpn-unknown = That device is not one of yours here.
net-device-vpn-cannot-share = This device cannot be a VPN for your others.
net-wrong-password = That password did not work. Check it and try again.
net-prompt-join = Connect to { $ssid }
net-prompt-new-password = New password for { $ssid }
net-hotspot-password-caption = Devices joining the hotspot need it. At least 8 characters.
net-prompt-use = Use
net-vpn-private-key-caption = The private key you were given for this tunnel.
net-vpn-private-key-wrong = That is not a WireGuard private key.
net-prompt-changing-password = Changing the password…
net-secret-change = Change…
net-secret-enter = Enter…
net-secret-given = Set
net-secret-none = Not set
net-vpn-none = No VPN on this device yet.
net-vpn-off = Off
net-vpn-unread = Unreadable
net-vpn-unknown = No VPN is called { $name }.
net-vpn-busy = { $name } is already changing.
net-vpn-connecting = Connecting { $name }…
net-vpn-disconnecting = Disconnecting { $name }…
net-vpn-connecting-short = Connecting…
net-vpn-disconnecting-short = Disconnecting…
net-vpn-connected = { $name } is connected.
net-vpn-disconnected = { $name } is disconnected.
net-vpn-switch-err = Could not switch { $name }: { $error }
net-vpn-kind-other = VPN
net-vpn-gone = This VPN is no longer on this device.
net-vpn-read-err = Could not read the VPN's settings: { $error }
net-vpn-name = Name
net-vpn-all-traffic = Send all traffic
net-vpn-all-traffic-caption = Off, only the networks the VPN routes go through it.
net-vpn-mtu-caption = The largest packet it sends.
net-vpn-mtu-automatic = Automatic
net-vpn-mtu-custom = Custom
net-vpn-mtu-size = Packet size
net-vpn-mtu-range = The MTU is 576 to 9000 bytes.
net-vpn-dns-remove = Remove
net-vpn-dns-own = It asks the name servers the VPN gives.
net-vpn-dns-add = Add server
net-vpn-dns-add-placeholder = IP address
net-vpn-dns-not-ip = { $server } is not an IP address.
net-vpn-settings-unread = The VPN's settings have not been read yet.
net-vpn-address = Address in the tunnel
net-vpn-server = Server
net-vpn-server-key = Server's public key
net-vpn-allowed = Routed addresses
net-vpn-keepalive = Keepalive (seconds)
net-vpn-keepalive-caption = Keeps the tunnel open behind a router; 0 sends none.
net-vpn-export = Export
net-vpn-export-caption = Save the VPN as a file to set it up elsewhere. The file holds its keys.
net-vpn-export-title = Export VPN
net-vpn-exported = VPN exported.
net-vpn-export-err = Could not export the VPN: { $error }
net-vpn-delete = Delete VPN
net-vpn-deleted = Deleted { $name }.
net-vpn-delete-err = Could not delete { $name }: { $error }
net-vpn-import-file = Import from file…
net-vpn-import-caption = A WireGuard .conf or OpenVPN .ovpn file
net-vpn-import-title = Import VPN
net-vpn-import = Import
net-vpn-importing = Importing the VPN…
net-vpn-import-err = Could not import the VPN: { $error }
net-vpn-file-unreadable = The file could not be read as text.
net-vpn-file-unknown = Choose a WireGuard .conf or OpenVPN .ovpn file.
net-vpn-file-wrong-kind = This file is a different kind of VPN.
net-vpn-new-wireguard = New WireGuard tunnel
net-vpn-new-plugin = New { $kind } VPN
net-vpn-no-plugin = That kind of VPN is not installed.
net-vpn-plugin-caption = The VPN's other settings are its plugin's own.
net-vpn-keys = Keys
net-vpn-keys-caption = Make a key pair here, or paste the private key you were given.
net-vpn-generate-keys = Generate keys
net-vpn-public-key = Public key
net-vpn-private-key = Private key
net-vpn-create = Create
net-vpn-creating = Setting up the VPN…
net-vpn-created = VPN added.
net-vpn-create-err = Could not add the VPN: { $error }
net-vpn-needs-name = Enter a name for the VPN.
net-vpn-needs-server = Enter the VPN's server.
net-vpn-paste-title = Paste a configuration
net-vpn-paste-caption = Paste the text of a WireGuard configuration file.
net-vpn-config = Configuration
perm-privacy-hardware = Privacy hardware
perm-camera = Camera
perm-microphone = Microphone
perm-mic-mute = Microphone mute
perm-muted = Muted
perm-listening = Listening allowed
perm-unavailable = Unavailable
perm-kill-hw = Blocked by hardware
perm-kill-sw = Blocked by software
perm-kill-allowed = Available
perm-kill-present = Present, no switch reported
perm-kill-missing = Not found
perm-active-use = Active privacy use
perm-no-active-use = No application is using the camera or microphone now.
perm-using-camera = Using camera
perm-using-mic = Using microphone
perm-device-security = Device security
perm-disk = Disk encryption
perm-secure-boot = Secure Boot
perm-firewall = Firewall
perm-encrypted = Encrypted
perm-not-encrypted = Not encrypted
perm-enabled = Enabled
perm-disabled = Disabled
perm-firewall-firewalld = Active, firewalld
perm-firewall-nftables = Active, nftables
perm-firewall-off = Not active
perm-workspace-intro-title = Workspace access
perm-open-workspace = Open workspace access
perm-no-grants-title = No workspace access
perm-no-grants-body = No app can reach anything outside its own workspace. When you approve access at a prompt, it appears here with what it covers and how long it lasts.
perm-unavailable-title = Workspace access can't be checked
perm-unavailable-body = Tilo could not read the workspace access record, so it cannot tell whether any app has extra access.
perm-unavailable-caption = Open workspace access, or try again once the desktop session has finished starting.
perm-grants-summary = { $grants } { $grants ->
        [one] grant
       *[other] grants
    } in { $workspaces } { $workspaces ->
        [one] workspace
       *[other] workspaces
    }
perm-workspace-unnamed = Workspace
perm-kind-files = Files
perm-kind-camera = Camera
perm-kind-mic = Microphone
perm-kind-screen = Screen sharing
perm-kind-remote = Remote control
perm-kind-network = Network
perm-kind-action = App action
perm-verb-rw = read & write
perm-verb-ro = read-only
perm-any-host = Any host
perm-action-detail = Use “{ $action }” on { $resource } in { $app }
perm-expiry-always = Always
perm-expiry-temp = Temporary
perm-expiry-logout = Until log out
perm-expiry-once = Once
perm-capture-video = Capture video
perm-capture-audio = Capture audio
perm-stream-workspace = Stream this workspace
perm-inject-input = Inject pointer & keyboard
perm-desktop-control = Use the desktop’s actions beyond its workspace
perm-kind-desktop-control = Desktop control
perm-desktop-control-title = Terminals controlling your desktop
perm-desktop-control-intro = A terminal tab you let control your desktop can use its actions beyond its workspace until the tab closes. Identity, security, updates and deleting data still ask each time.
perm-desktop-control-none = No terminal tab controls your desktop right now.
perm-desktop-control-tab = { $app }, tab “{ $tab }”
perm-desktop-control-until-closes = Until the tab closes
perm-desktop-control-workspace = From { $workspace }
perm-desktop-control-stop = Stop
power-source-title = Power source
power-source = Source
power-source-ac = AC power
power-source-caption = This device has no battery. It runs on mains power, so charge and battery-saver settings don't apply.
power-battery-title = Battery
power-cycles = Charge cycles
power-health = Battery health
power-health-unknown = Not reported by this device
power-history-title = Charge, last 30 days
power-charge-cap = Charge cap
power-charge-cap-unsupported = Not supported by this hardware
power-profile-title = Power profile
power-profile = Profile
power-profile-unavailable = Power profiles aren’t available on this device
power-reading = Reading the power hardware…
power-profile-saver = Power Saver
power-profile-balanced = Balanced
power-profile-performance = Performance
power-idle-title = Automatic power saving
power-suspend-idle = Suspend when idle
power-suspend-after = Suspend after
power-suspend-battery = Suspend on battery after
power-suspend-plugged = Suspend when plugged in after
power-suspend-needs-idle = Turn on Suspend when idle to choose this
power-suspend-in-profile = This device suspends after the time set for each power profile, under Power profile.
power-suspend-in-profile-value = Set for each power profile
power-blank = Blank screen when idle
power-lock-after = Lock after
power-behaviour-title = Power behaviour
power-lid = When the lid is closed
power-power-key = When the power key is pressed
power-critical = When the battery is critically low
power-warn-caption = You'll be warned at { $warn }% and this action runs at { $critical }%.
power-devices-title = Power devices
power-device-battery = Battery
power-device-ups = UPS
power-device-line = { $kind }, { $capacity }%, { $status }
power-never = Never
power-1-minute = 1 minute
power-5-minutes = 5 minutes
power-10-minutes = 10 minutes
power-15-minutes = 15 minutes
power-30-minutes = 30 minutes
power-45-minutes = 45 minutes
power-1-hour = 1 hour
power-action-suspend = Suspend
power-action-lock = Lock screen
power-action-ignore = Do nothing

power-status-full = Fully charged
power-state-left = On battery, { $time } left
power-state-to-full = Charging, full in { $time }
power-history-day = 24 hours
power-history-week = 7 days
power-history-span = Charge history span
power-history-name = Battery charge, last { $span }
power-history-at = { $day } { $time }
power-history-reading = { $charge } at { $time }
power-history-empty = No charge history yet
power-history-summary = Now { $last }, lowest { $low }, highest { $high }
power-state-charging = Charging
power-state-plugged = Plugged in, not charging
power-state-battery = On battery
power-health-none = This battery does not report comparable full and design capacity.
power-health-good = Capacity is close to the factory design value; normal variation is expected.
power-health-aged = Some capacity has been lost through normal chemical ageing.
power-health-reduced = Capacity is noticeably reduced; shorter runtime is expected.
power-health-severe = Capacity is severely reduced. Consider service or replacement if runtime is disruptive.
power-capacity-full = Capacity when full
power-capacity-of = { $full } of { $design }
power-watt-hours = { $amount } Wh
settings-printers-attention = Printer needs attention
settings-printers-card = Printers
settings-printers-nearby = Nearby
settings-printers-activity = Current activity
settings-printers-scanners = Scanners
settings-printers-find = Find printers
settings-printers-searching = Searching…
settings-printers-make-default = Make default
settings-printers-pause = Pause
settings-printers-resume = Resume
settings-printers-remove = Remove…
settings-printers-add = Add
settings-printers-retry = Retry
settings-printers-cancel = Cancel
settings-printers-empty = No printers yet. Choose Find printers to look on your network.
settings-printers-discovering = Looking for printers on your network…
settings-printers-discover-empty = Printers on your network that need no driver appear here.
settings-printers-no-jobs = No active print jobs.
settings-printers-no-scanners = No scanners found.
settings-printers-state-ready = Ready
settings-printers-state-printing = Printing
settings-printers-state-paused = Paused
settings-printers-state-offline = Offline
settings-printers-role-default = Default
settings-printers-role-configured = Configured
settings-printers-not-accepting = Not accepting new jobs
settings-printers-default-accepting = Default, accepting jobs
settings-printers-accepting = Accepting jobs
settings-printers-detail-reason = { $role }, { $reason }
settings-printers-supplies-none = No supplies installed
settings-printers-supplies-list = Supplies: { $list }
settings-printers-supply-level = { $name } { $level }%
settings-printers-supply-unknown = { $name }, level unknown
settings-printers-supplies-unreported = Supply levels are not reported by this printer
settings-printers-ipp-unavailable = Supply levels are not available
settings-printers-job-state-missing = Status unknown
settings-printers-job-progress = { $done } of { $total } pages, { $percent }%
settings-printers-job-done = { $done } pages done
settings-printers-job-progress-unknown = progress unknown
settings-printers-job-waiting = Waiting
settings-printers-job-sent = Sent to { $destination } by { $owner }, { $size }
settings-printers-job-held = Held
settings-printers-job-printing = Printing
settings-printers-job-stopped = Stopped
settings-printers-job-canceled = Canceled
settings-printers-job-failed = Failed
settings-printers-job-completed = Completed
settings-printers-job-unknown = Status unknown
settings-printers-worker-failed = Could not start printer status worker: { $error }
settings-printers-discover-failed = Could not start printer discovery: { $error }
settings-region-language-card = Language
settings-region-language = Language
settings-region-language-device = Same as this device
settings-region-language-installing = Installing { $language }. The desktop switches to it when it is installed.
settings-region-language-failed = The language could not be installed: { $error }
settings-region-language-no-installer = This device's installer is not available to Settings, so only the languages already installed are offered.
settings-region-language-body = Apps and the desktop show their words in this language. A word not yet translated shows in English.
settings-region-formats-card = Formats
settings-region-currency = Currency
settings-region-date-format = Date format
settings-region-time-format = Time format
settings-region-measurement = Measurement
settings-region-number-format = Number format
settings-region-first-day = First day of week
settings-region-date-language = Follow language
settings-region-date-iso = ISO (2023-03-15)
settings-region-date-us = US (03/15/2023)
settings-region-date-eu = European (15/03/2023)
settings-region-date-long = Long (15 Mar 2023)
settings-region-time-language = Follow language
settings-region-time-24 = 24-hour (14:05)
settings-region-time-12 = 12-hour (2:05 PM)
settings-region-measure-metric = Metric (°C, km, kg)
settings-region-measure-imperial = Imperial (°F, mi, lb)
settings-region-weekday-monday = Monday
settings-region-weekday-sunday = Sunday
settings-region-weekday-saturday = Saturday
settings-region-preview-card = Preview
settings-region-preview-amount = Amount
settings-region-preview-date = Date
settings-region-preview-time = Time
settings-region-preview-temp = Temperature
settings-region-preview-c = 21 °C
settings-region-preview-f = 70 °F

# Storage pane
settings-storage-use = Storage use
settings-storage-capacity-used = { $used } of { $total }
settings-storage-capacity-used-label = Used
settings-storage-capacity-available = Available
settings-storage-workspaces = Workspaces
settings-storage-kept-on = Kept on { $devices }
settings-storage-cat-apps = Installed apps
settings-storage-cat-data = App data
settings-storage-cat-cache = Cache
settings-storage-cat-synced = Synced files
settings-storage-on-device = On this device
settings-storage-remote-only = On other devices only
settings-storage-safe-to-free = Safe to free
settings-storage-synced-card = Synced storage
settings-storage-free = Free { $size }
settings-storage-loading = Loading synced storage…
settings-storage-cleaning = Removing application caches…
settings-storage-reclaiming = Verifying remote copies and freeing local space…
settings-storage-nothing-reclaimable = Nothing is safely reclaimable. Dirty, pinned, shared, and unverified content stays local.
settings-storage-offline = You are offline. Local synced content stays available and cannot be reclaimed.
settings-storage-accounting-unavailable = Sync storage accounting is not available.
settings-storage-service-unavailable = Sync storage service unavailable: { $error }
settings-storage-service-ended = Sync storage service ended unexpectedly.
settings-storage-freed-queued = Freed { $size } from { $evicted } synced file(s); { $queued } more queued for verified background reclamation.
settings-storage-queued-only = { $queued } synced file(s) are waiting to be removed. Their space counts as free once they are gone.
settings-storage-freed = Freed { $size } from { $evicted } synced files.
settings-storage-reclaim-done = Synced-file reclamation completed.
settings-storage-nothing-removed = Nothing was removed: { $error }
settings-storage-request-ended = The sync storage request ended unexpectedly; nothing was assumed removed.
settings-storage-cleaned = Application caches cleaned.
settings-storage-clean-failed = Could not clean application caches: { $error }
settings-storage-no-cache = No reclaimable application cache is available.
settings-storage-no-reclaimable = No reclaimable synced files are available.
settings-storage-opened-disks = Opened Disks.
settings-storage-opened-backups = Opened Backups.
settings-storage-largest = Largest items
settings-storage-no-apps = No Tilo application storage was found on this device.
settings-storage-remove = Remove { $size }
settings-storage-cache-item = Cache item
settings-storage-cleanup-preview = Cache cleanup preview
settings-storage-cleanup = Cache cleanup
settings-storage-cleanup-empty = There is no application cache to clean.
settings-storage-reclaimable = { $size } to clean up
settings-storage-clean-nothing = Nothing to clean up
settings-storage-tools = Storage tools
settings-storage-disks = Disks
settings-storage-disks-desc = Partitions, formatting, drive health, and removable media
settings-storage-open-disks = Open Disks
settings-storage-backups = Backups
settings-storage-backups-desc = Backup history, schedules, retention, and restore
settings-storage-open-backups = Open Backups
settings-storage-cache-card = Workspace files on this device
settings-storage-prefetch = Prefetch files
settings-storage-prefetch-desc = Download the files you are likely to open next before you open them
settings-storage-cache-size = Space for fetched files
settings-storage-cache-size-auto = Automatic
settings-storage-cache-size-gb = { $size } GB
settings-storage-cache-size-unknown = Unknown cache size: { $size }
settings-storage-cache-caption = Prefetching pauses on metered networks, low battery and a full disk. Files kept offline and files only on this device never count against this space.
settings-storage-cache-changed = Space for fetched files changed.
settings-storage-cache-failed = Could not change how files are kept: { $error }
time-clock-title = Clock
time-auto = Set time automatically
time-timezone = Time zone
time-zone-automatic = Set automatically
time-zone-automatic-now = Set automatically ({ $zone })
time-zone-automatic-caption = The time zone follows this device's location.
time-zone-name = { $city } ({ $region })
time-current-time = Current time
time-current-date = Current date
time-source-title = Time source
time-sync = Synchronization
time-sync-ok = Synchronized
time-sync-waiting = Waiting for synchronization
time-sync-disabled = Automatic time is disabled
time-sync-unknown = Unknown
time-source = Source
time-source-unknown = Unknown
time-notice-title = That did not work
upd-notice-title = That did not work
upd-os-current = Up to date
upd-firmware = Device firmware
upd-software = Software updates
upd-ready = { $version } is ready to install
upd-restart-update = Restart & update
upd-current-version = Current version
upd-last-checked = Last checked
upd-automatic = Automatic updates
upd-schedule = Download schedule
upd-schedule-system = System schedule
upd-schedule-daily = Daily
upd-schedule-weekly = Weekly
upd-metered = Download on metered connections
upd-check = Check for updates
upd-checking = Checking for updates…
upd-lifecycle = Recovery
upd-rollback-health = Rollback protection
upd-release-notes = Release notes
upd-fw-status = Status
upd-fw-current-status = Device firmware is up to date
upd-restart-required = Restart required
upd-restart-now = Restart now
upd-check-firmware = Check firmware
upd-checking-firmware = Checking firmware…
upd-update-to = Update to { $version }
upd-update-available = Update available
upd-before-installing = Before installing: { $problems }
upd-install-firmware = Install firmware upgrades
upd-installing = Installing
upd-installation = Installation
upd-installed-reboot = Installed, restart required
upd-installed-ok = Installed successfully
upd-fw-history = Firmware history
upd-fw-history-empty = No firmware updates have been installed on this device.
upd-fwupd-error = Update failed: { $error }
upd-history-query-failed = Firmware history is not available on this device
upd-rollback-title = Go back to the previous image
upd-no-previous = There is no previous image on this machine yet.
upd-no-rollback-caption = A previous version becomes available after the next update.
upd-previous-image = Previous image: { $image }
upd-image-built = { $image }, built { $date }
upd-rollback-intro = Rolling back makes the previous image the one this machine boots. Nothing is deleted: the image you are on now stays on the disk and in the boot menu.
upd-roll-back = Roll back…
upd-roll-back-to = Roll back to { $image }?
upd-caveat-image = The system image reverts: its programs, the compositor and the apps that ship with it.
upd-caveat-files = Your files and app data do NOT revert. Workspaces, documents, keys and downloads are shared by every image on this disk, so a rollback neither restores them nor loses them.
upd-caveat-apps = Apps you installed or updated yourself are kept. One made for the newer system runs as the previous image ships it, or fetches what it needs at the next app update; rolling forward brings it back as it was.
upd-roll-back-later = Roll back and restart later
upd-help-title = How updates work
upd-help-2 = If a new image fails to reach a login screen three times in a row, this machine goes back to the previous one by itself. If there is nothing to go back to, it starts in Repair Mode instead.
upd-codecs = Play more video and audio formats
upd-codecs-explain = Downloaded to this device from the formats' publishers. You can turn it off here at any time.
upd-status = Status
upd-progress = Progress: { $percent }%
upd-installing-codecs = Installing more formats
upd-codecs-removing = Removing…
upd-codecs-checking-driver = Checking for hardware support…
upd-codecs-starting = Starting…
upd-codecs-not-installed = Off
upd-codecs-waiting = Waiting for an internet connection. It turns on by itself once this device is online.
upd-codecs-step = { $label } (step { $step } of { $steps })…
upd-codecs-step-bytes = { $label } { $done } of { $total } MB (step { $step } of { $steps })…
upd-codecs-downloading = Downloading…
upd-codecs-failed = Could not install
upd-codecs-failed-reason = Could not install: { $error }
upd-codecs-installed-hw = On, including hardware H.264/HEVC
upd-codecs-installed-mesa = On. Hardware H.264/HEVC follows once a matching driver is published.
upd-codecs-installed-sw = On
upd-codecs-refresh-failed = The last update of the formats failed: { $error }
upd-codecs-applies-hw = Apps you open from now on play and record H.264 and HEVC with hardware acceleration. Sign out and back in to use it for screen sharing and recording, and reopen apps that were already open.
upd-codecs-applies-sw = Works right away: calls, screen sharing and videos pick it up the next time they start. No restart is needed.
upd-codecs-install = Install
upd-codecs-try-again = Try again
upd-codecs-cancel = Cancel
upd-codecs-remove = Remove
upd-codecs-refresh = Check for hardware support again
upd-notice-schedule = Download schedule could not be changed: { $error }
upd-notice-metered = Metered-network policy could not be changed: { $error }
upd-notice-codecs = The formats could not be changed: { $error }
upd-notice-fw-reboot = Firmware installation finished. Restart to activate the update.
upd-notice-fw-ok = Firmware installation finished successfully.
upd-notice-fw-failed = Firmware installation failed: { $error }
upd-notice-check-stopped = Updates could not be checked: worker stopped
upd-notice-check = Updates could not be checked: { $error }
upd-notice-fw-refresh = Firmware metadata could not be refreshed: { $error }
upd-notice-fw-busy = Firmware work is already in progress
upd-notice-fw-install = Firmware upgrades could not be installed: { $error }
upd-notice-restart = Restarting into the staged update failed: { $error }
upd-notice-auto = Automatic updates could not be changed: { $error }
upd-notice-rollback-ok = Done. The previous image is what this machine will boot next. Restart when you are ready. Nothing has been removed, so this is reversible.
upd-notice-rollback = Rolling back failed: { $error }
upd-fw-preparing = Preparing { $total } update(s)…
upd-fw-preparing-tx = Preparing firmware transaction…
upd-health-protected = Protected, can go back to { $target }
upd-health-confirm = Protected, no previous image yet
upd-health-fallback = Can go back to { $target }
upd-health-unknown = Not reported
upd-staged-generic = Update staged for next boot

# Values a pane could not read. Each replaces the value it stands for, so a
# failed read is never shown as a state of the device.

settings-programs-error = Can't check which programs are installed: { $error }
net-status-error = Can't read network status: { $error }
net-wired-error = Can't read the wired connection: { $error }
net-radios-error = Can't read the radio state: { $error }
net-hotspot-clients-error = Can't list connected devices: { $error }
net-vpn-error = Can't read VPN connections: { $error }
net-profiles-error = Can't read saved networks: { $error }
upd-status-error = Can't check for updates: { $error }
upd-rollback-error = Can't read the previous system image: { $error }
time-status-error = Can't read time settings: { $error }
time-zones-error = Can't list time zones: { $error }
time-reading = Reading the clock…
settings-users-read-error = Can't read this device's accounts: { $error }
settings-users-identities-read-error = Can't read your identities: { $error }
settings-users-autologin-error = Can't read automatic login: { $error }
settings-bluetooth-read-error = Can't read Bluetooth: { $error }
settings-bluetooth-no-adapter = No Bluetooth adapter on this device
sound-devices-error = Can't list audio devices: { $error }
sound-volume-error = Can't read the output volume: { $error }
sound-input-error = Can't read the input level: { $error }
display-brightness-error = Can't read the brightness: { $error }
perm-firewall-error = Can't read the firewall state: { $error }
mesh-runtime-unreadable = Can't read
mesh-home-unreadable = Settings can't see this device's sync service, so whether it runs is unknown.
input-catalog-error = Showing a short built-in list. Can't read this device's keyboard layouts: { $error }
power-profile-unread = Current profile unknown
settings-printers-read-error = Can't list printers: { $error }
settings-printers-jobs-read-error = Can't list print jobs: { $error }
settings-printers-scanners-read-error = Can't list scanners: { $error }

display-host-observation-timeout = The display preview status could not be refreshed. Inspect the current configuration before continuing.
shortcut-name-dictation-toggle = Start or finish dictation
shortcut-name-dictation-finish = Finish dictation
shortcut-name-dictation-cancel = Cancel dictation
shortcut-name-tab-menu = Show tab menu

# Network connection guidance
net-wifi-caption = Choose a network to connect. Refresh searches for nearby networks.
net-diag-idle = Run diagnostics to check your connection, address and DNS.
net-hotspot-clients-caption = Devices currently connected to your hotspot.
net-share-caption = Share a saved Wi-Fi network securely with your other devices.
net-ipv4-caption = Use automatic addressing unless your network requires a fixed address.

net-wake-network = Wake over { $name }
net-wake-disabled = Enable magic-packet wake for this connection. Whether this machine resumes must be tested separately.
net-wake-unverified = Magic-packet wake is saved and enabled in the driver. Waking this machine has not been verified.
net-wake-connect-first = Connect to this network to check its device and apply wake settings.
net-wake-unsupported = This connection's driver does not report magic-packet wake support.
net-wake-unavailable = Wake settings could not be read.
net-wake-pending = The wake setting is saved, but applying it to this connection needs attention.
net-wake-saved = Wake configuration saved. Physical wake remains unverified.

# Copy used by Settings panes and their status/error branches.
a11y-mouse-keys-caption = Use the numeric keypad to move the pointer and click.
a11y-shared-caption = Tilo apps use these shared accessibility settings.
a11y-shared-title = Across Tilo
a11y-text-size = Text size
accounts-form-caption = These details are saved for this account.
accounts-nextcloud-calendars = Connect calendars
accounts-nextcloud-calendar-caption = Uses this Nextcloud account’s existing sign-in. Choose which calendars to sync, then save to confirm. Files and mail stay unchanged.
accounts-calendar-discover = Find calendars
accounts-calendar-details-changed = Account details changed during discovery. Find calendars again to check the new details.
accounts-calendars-title = Choose calendars
accounts-calendars-caption = Review the calendars to connect. New setups select all readable calendars; existing selections are kept. Only selected calendars will sync. Nothing is saved until you confirm.
accounts-calendar-select = Select
accounts-calendar-deselect = Deselect
accounts-calendar-read-only = Read-only calendar
accounts-calendars-empty = Select at least one calendar. If none were found, check the server address and calendar access.
accounts-intro = Connect accounts for mail, calendars, contacts and cloud storage.
accounts-kind-empty = No { $kind } accounts yet.
accounts-reauth-caption = Replace the saved credential without removing the account.
accounts-sync-none = No synchronization status is available yet.

display-caption-port = Connect another display to an available port to arrange it here.
display-caption-second = Connect another display to extend your workspace.
display-caption-switch = Drag displays to match their physical arrangement.
display-card-title = Display arrangement
display-night-light-caption = Warmer colors reduce blue light in the evening.
mesh-activity-caption = Continue workspace and app activity on your other devices.
mesh-audit-caption = Recent changes show how device continuity was configured.
mesh-audit-change = Change
mesh-privacy-stay = Only the enabled categories are shared with your devices.
mesh-roaming-caption = These settings follow your identity across connected devices.
notif-app-verified = App identity
notif-delivery = Delivery
notif-dnd-title = Quiet notifications
notif-priority-caption = Priority contacts come from your contact preferences.
notif-sounds = Notification sounds
notif-sounds-pipewire = Available
notif-status-active = Notifications active
notif-status-banners-off = Banners off
notif-status-silenced = Do Not Disturb
perm-enforcement-caption = Tilo checks access at the workspace and host service boundaries. Removing a grant withdraws that access.
perm-secrets-title = Saved passwords and keys
perm-secrets-intro = Applications that stored or read saved passwords and keys on this device. Stopping one denies it all saved secrets; its own items stay until you forget it.
perm-secrets-unavailable = The saved-secrets service can't be reached, so its applications can't be listed.
perm-secrets-none = No application has used saved passwords or keys yet.
perm-secrets-detail = { $items } saved, used { $uses } times
perm-secrets-forget = Forget and delete its secrets
perm-secrets-access = Access to saved passwords and keys
perm-enforcement-title = How access is enforced
perm-no-grants-caption = Access outside a workspace requires an explicit grant.
perm-security-caption = Device security and workspace access are managed separately.
perm-this-device = This device
perm-this-device-body = These security settings describe this device. They do not change permissions in other workspaces.
perm-workspace-intro-body = Review the access approved for apps in each workspace. Manage collaborators and roles inside that workspace.
power-charge-cap-conservation = This device uses its battery conservation mode.
power-charge-cap-huawei = This device controls charging through its supported charge limits.
power-charge-cap-start-stop = Charging starts below the lower threshold and stops at the selected limit.
power-charge-cap-stop = Charging stops at the selected limit.
power-idle-caption = These timers apply when you are not interacting with this device.
power-profile-caption = Choose how this device balances performance and energy use.
settings-about-backup-caption = Back up your identity so you can recover it on another device.
settings-bluetooth-audio-caption = Manage sound output and input in Sound settings.
settings-bluetooth-refresh-failed = Could not refresh Bluetooth devices: { $error }
settings-bluetooth-trust-caption = Trusted devices can reconnect without pairing again.
settings-bluetooth-name = Name
settings-bluetooth-my-devices = My devices
settings-bluetooth-nearby-devices = Nearby devices
settings-personal-apps-choice-follows = Your preferred app is used unless the workspace has its own choice.
settings-personal-apps-scope-caption = Workspace app defaults are managed in that workspace.
settings-personal-apps-workspace-rules = This workspace's app rules
settings-personal-apps-open = Open
settings-printers-driverless = Driverless printer
settings-region-preview-week = Week starts on
settings-region-shared-body = Apps use these formats for dates, times, numbers and measurements.
settings-region-shared-card = Shared formats
settings-storage-cleanup-caption = Review the cache files below before removing them.
settings-storage-item-meta = { $size } · { $category }
settings-storage-tools-caption = Use Backups to protect files before making storage changes.
settings-storage-this-device = { $device } (this device)
settings-storage-this-device-unnamed = This device
settings-storage-no-workspaces = No synced workspace is stored here.
settings-storage-only-here-mark = { $size } only here
settings-storage-kept-here-mark = Kept here
settings-storage-only-here = On no other device: { $size } in { $files ->
    [one] one file
   *[other] { $files } files
}
settings-storage-replicated = Also on another device: { $size }
settings-storage-pinned = Kept here, also on another device: { $size }
settings-storage-cache = Cache, safe to free: { $size }
settings-storage-nothing-here = Nothing of it is stored here.
settings-storage-free-cache = Free { $size }
settings-storage-keep-here = Keep here
settings-storage-stop-keeping = Stop keeping here
settings-storage-cache-freed = Freed { $size } of cache from { $files ->
    [one] one file
   *[other] { $files } files
}.
settings-storage-now-kept = The workspace is kept on this device.
settings-storage-no-longer-kept = The workspace is no longer kept on this device.
settings-storage-residency-busy = Another storage change is still running.
settings-users-boundary-body = Device accounts control signing in here. Workspace membership and identity sharing are managed separately.
settings-users-identity-shares = { $threshold } of { $total } shares
settings-users-protection-hardware = Hardware-backed identities keep their key protection on the device.
settings-users-protection-secret-service = Identity secrets can be protected by this account's secret service.
settings-users-signed-in = Signed in
sound-echo-caption = Reduce playback sound picked up by the microphone.
sound-mic-caption = Test the selected microphone without changing app permissions.
sound-mono-caption = Combine left and right channels for the selected output.
sound-notice-device-gone = This sound device is no longer available.
sound-notice-stream-gone = This audio stream has ended.
sound-notice-theme-missing = This sound theme is no longer available.
sound-system-caption = Choose which system events play a sound.
speakers-name-join-note = The saved name identifies this speaker or room when joining it.
time-map-empty = No time-zone map locations are available.
upd-details = Update details
upd-fw-caption = Firmware updates come from this device's firmware service. Follow its restart instructions when an update is ready.
upd-fwupd-reported = The firmware service reported: { $error }
upd-help-1 = An OS update prepares a new system image. The current image stays available until the new one is activated.
upd-release-channel = Image source
upd-release-channel-value = Configured for this device
upd-restart-apply = Restart when convenient to apply the prepared system update.
upd-up-to-date = No update is ready to apply.
settings-appearance-theme = Theme
settings-appearance-density-desc = How much room controls and rows leave around them
settings-appearance-theme-automatic = Automatic
settings-appearance-theme-dark = Dark
settings-appearance-theme-light = Light
settings-appearance-theme-dark-graphite = Dark graphite
settings-appearance-theme-dark-cool = Dark cool
settings-appearance-theme-dark-soft = Dark soft
settings-appearance-desktop-card = Desktop
settings-appearance-hide-top-bar = Hide the top bar until the pointer is near it
settings-appearance-hide-top-bar-desc = Windows use the whole screen. The bar appears when you move the pointer to the top edge. Applies to this device only.
settings-appearance-schedule-follow = Follow the day and night schedule
input-map-title = Keyboard layout map
input-map-layout = Preview any installed layout
input-map-model = Physical keyboard model
input-map-ansi = 104-key ANSI
input-map-iso = 105-key ISO
input-map-instructions = Press keys while this page is open, including while searching layouts. The map highlights physical positions without changing what you type.
input-map-levels = Choose a level to see what each key types with Shift or AltGr.
input-map-unavailable = This layout can’t be previewed.
input-map-apply = Use this layout
input-map-extra-levels = Show levels 5–8 when the layout has them
input-map-dead-keys = Dead keys use a dotted circle.

input-map-jis = 106-key Japanese
input-map-korean = 106-key Korean
input-map-live = Show live keyboard
input-map-level = Modifier level

input-map-live-description = While Settings is focused, this map follows the keyboard’s current layout and modifiers.
input-map-preview-description = Previewing the selected layout. Choose a level or inspect a key to see its other symbols.

input-map-inspect-key = Inspect a physical key
input-map-inspection-unavailable = This physical key’s current symbols are unavailable. Focus Settings or choose a layout preview.
input-map-current-symbols = Current symbols

a11y-dictation-language = Dictation language
a11y-dictation-language-desc = Choose a language to improve recognition and reduce waiting. Automatic detection covers other supported languages. Dictation uses your system microphone from Sound settings and asks before each recording.
dictation-language-auto = Automatic detection
dictation-language-en = English
dictation-language-de = German
dictation-language-fr = French
dictation-language-es = Spanish
dictation-language-ar = Arabic
dictation-language-zh = Mandarin Chinese
dictation-language-ja = Japanese
dictation-language-it = Italian
dictation-language-pt = Portuguese
dictation-language-hi = Hindi
dictation-language-ru = Russian
dictation-language-uk = Ukrainian
dictation-language-ko = Korean
dictation-language-tr = Turkish
dictation-language-pl = Polish
dictation-language-id = Indonesian
dictation-language-vi = Vietnamese
dictation-language-nl = Dutch
dictation-language-bn = Bengali
dictation-language-he = Hebrew
dictation-language-fa = Persian
dictation-language-th = Thai
dictation-language-sv = Swedish
dictation-language-cs = Czech
dictation-language-el = Greek
dictation-language-ta = Tamil
dictation-language-ur = Urdu
dictation-language-hu = Hungarian
dictation-language-ro = Romanian
dictation-language-fi = Finnish
display-devices-title = Screens on your devices
display-device-screen = { $device } · { $screen } · { $status }
display-device-online = Online
display-device-offline = Offline
display-device-switched-off = Switched off on that device
display-devices-observed = Observed screen positions. Input access is granted separately.

display-devices-arrange = Drag an online device screen beside yours. Screens on one device move together. Moving screens does not allow input.
display-device-move-requested = Placement requested. The peer layout updates when it confirms its new position.
display-device-move-pending = Sending display placement…
display-device-move-unavailable = That screen is offline or its arrangement cannot be changed here.

display-device-move-confirmed = The device confirmed its new display position.
display-device-move-changed = The device layout changed before this position was confirmed. Try again.
display-device-move-unconfirmed = The device did not confirm this position. Refresh and try again.

a11y-dictation-literal = Literal dictation
a11y-dictation-literal-desc = Keep recognised words as spoken. When off, explicit number corrections in English, German, French or Spanish are removed, such as “two, uh, no, three” becoming “three”. Names, quoted speech and other disfluencies stay unchanged.
input-dictionary-title = Personal dictionary
input-dictionary-caption = Words you add are available to spelling, the on-screen keyboard and on-device dictation. Your spelling and capitalization are preserved.
input-dictionary-unavailable = Your local personal dictionary is unavailable in this session.
input-dictionary-word = Word
input-dictionary-language = Language
input-dictionary-language-caption = Leave language empty for all languages, or enter a language tag such as en, nl or fr.
input-dictionary-add = Add word
input-dictionary-save = Save
input-dictionary-remove = Remove
input-dictionary-reset = Remove all words
input-dictionary-word-placeholder = Enter a word
input-dictionary-all-languages = All languages

input-scope-all = All keyboards
input-scope-inheritance = Every keyboard types these layouts unless you give it its own. The on-screen keyboard has its own.
input-scope-own = { $name } types its own layouts
input-scope-use-shared = Use shared layouts
input-error-keyboard-shared = that keyboard already types the shared layouts.
input-keyboard-scope = Applies to
input-source-activate = Make active
input-sources-selection-description = Select a source to preview or edit. Drag to reorder.
input-sources-back = Back to sources
input-sources-cancel = Cancel
input-sources-unavailable = Your layouts couldn’t be shown.
input-sources-loading = Loading layouts…
input-osk-qwerty = English (QWERTY)
input-osk-azerty = French (AZERTY)
input-osk-preview-description = The touch keyboard uses this letter layout; physical compose and Caps Lock options are separate.
input-source-options = Source options
input-source-inspection = Key inspection

input-keyboard-numbered = { $name } ({ $number })

input-source-show = Show
input-source-hide = Hide

input-source-add-short = Add
input-source-add-title = Add a layout

# Keyboard source editing errors.
input-error-keyboard-retained = the keyboard is no longer available; its assignment has been retained
input-error-keyboard-unavailable = the keyboard is not currently available
input-error-source-unconfigured = the source is not configured for this keyboard
input-error-source-add-unavailable = the source is not available in the Add screen
input-error-source-choose = choose a source to add
input-error-source-physical-options = physical keyboard options do not apply to the on-screen keyboard
input-error-source-compose = invalid compose key
input-error-source-caps = invalid Caps Lock behaviour
input-error-source-option = invalid keyboard option
input-error-source-missing = the source is not configured
input-error-source-add = the source cannot be added to this keyboard
input-error-source-last = the last source cannot be removed
input-error-source-empty = no source remains
input-error-source-first = the source is already first
input-error-source-end = the source is already last
input-error-source-overflow = source index overflow
input-error-source-operation = this is not a source operation
input-error-live-map-physical = Live physical-keyboard inspection is unavailable for the on-screen keyboard.

settings-disk-unlock-title = Disk unlock
settings-disk-unlock-intro = After a restart, an encrypted disk enrolled here can open with the approval of one of your other devices instead of the passphrase.
settings-disk-unlock-loading = Reading how this computer's disks unlock…
settings-disk-unlock-read-error = Can't read how this computer's disks unlock: { $error }
settings-disk-unlock-none = No encrypted disk on this computer unlocks with your other devices.
settings-disk-unlock-volume = Encrypted disk { $id }
settings-disk-unlock-secrets = Unlock secrets
settings-disk-unlock-sealed = Sealed to the TPM
settings-disk-unlock-sealed-body = This computer's half of the unlock key and the Wi-Fi passwords below are sealed to its TPM. Only this computer, starting software Tilo signed, can read them.
settings-disk-unlock-cleartext = Cleartext in the disk header
settings-disk-unlock-cleartext-body = This computer has no TPM it can use, so its half of the unlock key and the Wi-Fi passwords below are stored unencrypted in the disk header. Anyone holding this disk can read them. They cannot open the disk on their own: one of your devices still has to approve.
settings-disk-unlock-siblings = Devices asked at startup
settings-disk-unlock-no-siblings = None
settings-disk-unlock-networks = Wi-Fi for unlocking
settings-disk-unlock-networks-sealed = Wired connections are always tried. Startup joins only the Wi-Fi networks used here; their passwords are sealed to the TPM.
settings-disk-unlock-networks-cleartext = Wired connections are always tried. Startup joins only the Wi-Fi networks used here. Anyone holding this disk can read their passwords.
settings-disk-unlock-wifi-error = Can't list saved Wi-Fi networks: { $error }
settings-disk-unlock-no-wifi = No saved Wi-Fi network. Join one first to use it for unlocking.
settings-disk-unlock-use = Use
settings-disk-unlock-stop = Stop using
settings-disk-unlock-chosen = Used for unlock
settings-disk-unlock-unsaved = No longer a saved network on this computer.
settings-disk-unlock-changing = Changing the unlock networks…
settings-disk-unlock-change-error = The unlock networks were not changed: { $error }
settings-disk-unlock-saved = Startup can now unlock over { $networks } and wired connections.
settings-disk-unlock-wired-only = Startup now unlocks over wired connections only.
settings-disk-unlock-refreshed = Disk unlock refreshed.
settings-disk-unlock-busy = Another change to the unlock networks is still running.
settings-disk-unlock-not-authorised = Administrator approval was not given.
settings-disk-unlock-gone = That disk no longer unlocks with your other devices.
settings-disk-unlock-bad-request = That request names no usable disk or network.
settings-disk-unlock-too-many = The disk unlock answer lists more disks than this computer can have.
settings-disk-unlock-malformed = The disk unlock answer could not be read: { $error }

# Shared display arrangement, independent of device input permission.
display-shared-title = Your shared display layout
display-shared-description = Displays keep their device's layout. Inclusion and order follow you across devices; remote input requires separate permission.
display-shared-include = Include in the shared layout
display-shared-order = Display number
display-shared-read-only = Shared display inspection is read-only.
display-shared-unavailable = That display is no longer observed.
display-shared-stale = The arrangement changed. Refresh this display and try again.
display-shared-order-invalid = Choose a display number from the current list.
display-shared-save-failed = The arrangement could not be saved: { $error }
display-shared-saved = Shared display arrangement saved.
display-shared-move-stale = The device confirmed its position, but the shared arrangement changed. Refresh and try again.
display-shared-auto-input = Allow automatic input from your devices
display-shared-auto-description = While one of this device's displays is included, let your enrolled online devices control it. Continuity and Display/input must also be enabled. Devices you have declined stay declined. This choice applies only here.

input-layouts-title = Layouts
input-mismatch-title = Typing a different layout
input-mismatch-detail = This keyboard isn’t typing { $name }.
input-mismatch-apply = Switch
input-preview-in-use = In use
input-preview-previewing = Preview
input-layer-base = Base
input-layer-shift = Shift
input-layer-altgr = AltGr
input-layer-shift-altgr = Shift + AltGr
input-source-use = Use
input-source-layout = Layout
input-sources-reorder = Drag to reorder. Use switches the layout you type with.
input-source-add-placeholder = Search layouts
input-test-typing = Try it
input-test-typing-placeholder = Type here to test the layout and key repeat
input-map-configured = Show the configured layout
input-layer-base-short = Base
input-layer-shift-short = Shift
input-layer-altgr-short = AltGr
input-layer-shift-altgr-short = ⇧+AltGr

## History: what each change to a settings section did

settings-appearance-prefs-high-contrast = { $new ->
    [true] Turned on high contrast
   *[false] Turned off high contrast
}
settings-appearance-prefs-reduce-motion = { $new ->
    [true] Turned on reduced motion
   *[false] Turned off reduced motion
}
settings-appearance-prefs-reduce-transparency = { $new ->
    [true] Turned on reduced transparency
   *[false] Turned off reduced transparency
}
settings-appearance-prefs-forced-colors = { $new ->
    [true] Forced theme colours onto content
   *[false] Stopped forcing theme colours onto content
}
settings-accessibility-settings-large-cursor = { $new ->
    [true] Turned on the large cursor
   *[false] Turned off the large cursor
}
settings-accessibility-settings-sticky-keys = { $new ->
    [true] Turned on sticky keys
   *[false] Turned off sticky keys
}
settings-accessibility-settings-slow-keys = { $new ->
    [true] Turned on slow keys
   *[false] Turned off slow keys
}
settings-accessibility-settings-bounce-keys = { $new ->
    [true] Turned on bounce keys
   *[false] Turned off bounce keys
}
settings-accessibility-settings-bounce-keys-alert = { $new ->
    [true] Turned on the bounce keys alert
   *[false] Turned off the bounce keys alert
}
settings-accessibility-settings-mouse-keys = { $new ->
    [true] Turned on mouse keys
   *[false] Turned off mouse keys
}
settings-accessibility-settings-visual-alerts = { $new ->
    [true] Turned on visual alerts
   *[false] Turned off visual alerts
}
settings-accessibility-settings-cursor-crosshair = { $new ->
    [true] Turned on the pointer crosshair
   *[false] Turned off the pointer crosshair
}
settings-accessibility-settings-thick-text-caret = { $new ->
    [true] Turned on the thick text cursor
   *[false] Turned off the thick text cursor
}
settings-accessibility-settings-switch-control = { $new ->
    [true] Turned on switch control
   *[false] Turned off switch control
}
settings-input-prefs-tap-to-click = { $new ->
    [true] Turned on tap to click
   *[false] Turned off tap to click
}
settings-input-prefs-tap-drag = { $new ->
    [true] Turned on tap and drag
   *[false] Turned off tap and drag
}
settings-input-prefs-tap-drag-lock = { $new ->
    [true] Turned on the drag lock
   *[false] Turned off the drag lock
}
settings-input-prefs-disable-while-typing = { $new ->
    [true] Turned off the touchpad while typing
   *[false] Kept the touchpad on while typing
}
settings-input-prefs-middle-emulation = { $new ->
    [true] Turned on middle-click emulation
   *[false] Turned off middle-click emulation
}
settings-input-prefs-left-handed = { $new ->
    [true] Swapped the mouse buttons for the left hand
   *[false] Set the mouse buttons for the right hand
}
settings-input-prefs-touchpad-gestures = { $new ->
    [true] Turned on touchpad gestures
   *[false] Turned off touchpad gestures
}
settings-input-prefs-touchpad-enabled = { $new ->
    [true] Turned on the touchpad
   *[false] Turned off the touchpad
}
settings-input-prefs-touchpad-disabled-on-external-mouse = { $new ->
    [true] Turned off the touchpad while a mouse is connected
   *[false] Kept the touchpad on with a mouse connected
}
settings-text-assistance-prefs-check-spelling = { $new ->
    [true] Turned on spell checking
   *[false] Turned off spell checking
}
settings-text-assistance-prefs-auto-correct = { $new ->
    [true] Turned on auto-correct
   *[false] Turned off auto-correct
}
settings-power-prefs-auto-suspend = { $new ->
    [true] Turned on automatic suspend
   *[false] Turned off automatic suspend
}
settings-notification-prefs-do-not-disturb = { $new ->
    [true] Turned on Do Not Disturb
   *[false] Turned off Do Not Disturb
}
settings-notification-prefs-show-banners = { $new ->
    [true] Turned on notification banners
   *[false] Turned off notification banners
}
settings-notification-prefs-show-previews = { $new ->
    [true] Turned on notification previews
   *[false] Turned off notification previews
}
settings-notification-prefs-play-sounds = { $new ->
    [true] Turned on notification sounds
   *[false] Turned off notification sounds
}
settings-notification-prefs-show-on-lock-screen = { $new ->
    [true] Showed notifications on the lock screen
   *[false] Hid notifications on the lock screen
}
settings-notification-prefs-show-badges = { $new ->
    [true] Turned on notification badges
   *[false] Turned off notification badges
}
settings-notification-prefs-per-app = Changed how an app notifies
settings-app-notification-rule-banners = Changed an app's notification banners
settings-app-notification-rule-sounds = Changed an app's notification sounds
settings-app-notification-rule-badges = Changed an app's notification badges
settings-app-notification-rule-previews = Changed an app's message previews
settings-app-notification-rule-lock-screen = Changed an app's notifications on the lock screen
settings-app-notification-rule-priority = { $new ->
    [true] Let an app through during focus
   *[false] Stopped letting an app through during focus
}
settings-app-notification-rule-priority-contacts = { $new ->
    [true] Let an app's priority contacts through during focus
   *[false] Stopped letting an app's priority contacts through during focus
}
settings-app-notification-rule-muted-until = Changed how long an app is muted
settings-focus-schedule-enabled = { $new ->
    [true] Turned on the focus schedule
   *[false] Turned off the focus schedule
}
settings-focus-schedule-allow-priority = { $new ->
    [true] Let priority apps through during focus
   *[false] Stopped letting priority apps through during focus
}
settings-system-sound-prefs-enabled = { $new ->
    [true] Turned on system sounds
   *[false] Turned off system sounds
}
settings-system-sound-events-notification = { $new ->
    [true] Turned on the notification sound
   *[false] Turned off the notification sound
}
settings-system-sound-events-message = { $new ->
    [true] Turned on the message sound
   *[false] Turned off the message sound
}
settings-system-sound-events-volume-change = { $new ->
    [true] Turned on the volume change sound
   *[false] Turned off the volume change sound
}
settings-system-sound-events-screenshot = { $new ->
    [true] Turned on the screenshot sound
   *[false] Turned off the screenshot sound
}
settings-system-sound-events-device-connected = { $new ->
    [true] Turned on the device connected sound
   *[false] Turned off the device connected sound
}
settings-system-sound-events-device-disconnected = { $new ->
    [true] Turned on the device disconnected sound
   *[false] Turned off the device disconnected sound
}
settings-system-sound-events-low-battery = { $new ->
    [true] Turned on the low battery sound
   *[false] Turned off the low battery sound
}
settings-system-sound-events-error = { $new ->
    [true] Turned on the error sound
   *[false] Turned off the error sound
}
settings-display-prefs-night-light = { $new ->
    [true] Turned on night light
   *[false] Turned off night light
}
settings-display-prefs-night-light-schedule = { $new ->
    [true] Let night light follow day and night
   *[false] Stopped night light following day and night
}
settings-desktop-controls-vim-mode = { $new ->
    [true] Turned on Vim keys for the desktop
   *[false] Turned off Vim keys for the desktop
}
settings-appearance-prefs-mode = Set the appearance to { $new }
settings-appearance-prefs-dark-theme = Chose { $new } for dark mode
settings-appearance-prefs-light-theme = Chose { $new } for light mode
settings-appearance-prefs-density = Changed the spacing density
settings-appearance-prefs-text-scale = Changed the text size
settings-appearance-prefs-ui-scale = Changed the interface size
settings-appearance-prefs-schedule = Changed when day and night begin
settings-appearance-prefs-sunrise = Set day to begin at { $new }
settings-appearance-prefs-sunset = Set night to begin at { $new }
settings-appearance-prefs-wallpaper-scope = Changed which workspaces the wallpaper covers
settings-appearance-prefs-wallpaper-target = Changed where the wallpaper shows
settings-appearance-prefs-distinct-letters = Changed how similar letters are told apart
settings-accessibility-settings-cursor-size = Set the cursor size to { $new }
settings-accessibility-settings-on-screen-keyboard = Changed when the on-screen keyboard shows
settings-accessibility-settings-bounce-keys-delay-ms = Changed the bounce keys delay
settings-accessibility-settings-color-filter = Changed the colour filter
settings-accessibility-settings-magnifier-zoom = Set the magnifier to { $new }×
settings-accessibility-settings-magnifier-view = Changed how the magnifier shows the screen
settings-accessibility-settings-switch-key-pair = Changed the switch control keys
settings-accessibility-settings-switch-scan-ms = Changed the switch control scan
settings-input-prefs-touchpad-natural-scroll = Changed the touchpad scroll direction
settings-input-prefs-mouse-natural-scroll = Changed the mouse scroll direction
settings-input-prefs-pointer-speed = Changed the pointer speed
settings-input-prefs-touchpad-pointer-speed = Changed the touchpad speed
settings-input-prefs-mouse-pointer-speed = Changed the mouse speed
settings-input-prefs-key-repeat = Changed the key repeat
settings-input-prefs-accel-profile = Changed the pointer acceleration
settings-input-prefs-touchpad-click-method = Changed how the touchpad clicks
settings-input-prefs-touchpad-scroll-method = Changed how the touchpad scrolls
settings-input-prefs-scroll-speed = Changed the scroll speed
settings-text-assistance-prefs-spelling-language = Changed the spelling language
settings-power-prefs-suspend-minutes = Changed when the computer suspends
settings-power-prefs-saver-suspend-minutes = Changed when the computer suspends in power saver
settings-power-prefs-balanced-suspend-minutes = Changed when the computer suspends when balanced
settings-power-prefs-performance-suspend-minutes = Changed when the computer suspends in performance
settings-power-prefs-blank-minutes = Changed when the screen turns off
settings-power-prefs-lock-minutes = Changed when the screen locks
settings-power-prefs-lid-close-action = Changed what closing the lid does
settings-power-prefs-power-key-action = Changed what the power key does
settings-power-prefs-low-battery-percent = Set the low battery warning to { $new } %
settings-power-prefs-critical-battery-percent = Set the critical battery level to { $new } %
settings-power-prefs-critical-action = Changed what happens at critical battery
settings-focus-schedule-start-minute = Changed when focus begins
settings-focus-schedule-end-minute = Changed when focus ends
settings-focus-schedule-weekdays = Changed the focus days
settings-system-sound-prefs-volume = Changed the system sound volume
settings-system-sound-prefs-theme = Changed the sound theme
settings-audio-prefs-balance = Changed the left and right balance
settings-display-prefs-night-light-temperature = Changed how warm night light is
settings-display-prefs-scale = Changed the display scale
settings-locale-currency = Changed the currency
settings-locale-date-format = Changed the date format
settings-locale-time-format = Changed the time format
settings-locale-measurement = Changed the measurement system
settings-locale-number-style = Changed the number format
settings-locale-first-day-of-week = Changed the first day of the week
settings-choose-theme = Chose the theme { $theme }
settings-set-key-repeat = Changed the key repeat
settings-choose-layout = Chose the keyboard layout { $layout }
settings-mute-app = Changed how long an app is muted
settings-allow-continuity = { $on ->
    [true] Allowed Continuity between devices
   *[false] Stopped Continuity between devices
}
settings-allow-capability = { $on ->
    [true] Allowed { $capability } for Continuity
   *[false] Stopped { $capability } for Continuity
}

accounts-import-title = Import Thunderbird account settings
accounts-import-choose = Choose prefs.js
accounts-import-about = Review account identities, servers and sign-in methods. No passwords or tokens are imported. Accounts remain inactive until you explicitly sign in and authorize mail sync and sending.
accounts-import-reading = Reading account metadata…
accounts-import-review = Select the account settings to import and choose each sign-in method.
accounts-import-none = No IMAP accounts were found in these preferences.
accounts-import-auth-unspecified = Thunderbird did not specify every authentication method. Choose a supported sign-in method; no password method is assumed.
accounts-import-endpoints = IMAP { $imap }:{ $imap-port } ({ $incoming-transport }) · SMTP { $smtp }:{ $smtp-port } ({ $outgoing-transport })
accounts-import-logins = Incoming login: { $imap-user } · Outgoing login: { $smtp-user }
accounts-import-identity = { $name } — { $email }
accounts-import-password-choice = Select for password sign-in
accounts-import-oauth-choice = Select for browser sign-in
accounts-import-unselect = Deselect account
accounts-import-continue = Import selected settings
accounts-import-cancel = Cancel review
accounts-import-saved = Imported { $count } new account settings. Existing accounts were preserved. Each imported account still needs explicit sign-in.
accounts-import-password-step = Enter a new password or app password directly here, then save to authorize this account's mail sync and sending. No Thunderbird credential is copied.
accounts-import-oauth-step = Open Mail, edit this account and choose browser sign-in. Microsoft will ask for IMAP mail access, SMTP sending and offline access. No sign-in is performed by metadata import.
accounts-import-stale = This review changed. Read the current account review and try again.
accounts-import-accounts-changed = Accounts changed during review. Discover and review the metadata again.
accounts-import-auth-required = Choose an explicit supported authentication method.
accounts-import-select = Select at least one account before continuing.
accounts-import-save-failed = Account settings could not be saved. No credentials were imported.
accounts-import-read-failed = Thunderbird account metadata could not be read.
accounts-import-prefs-required = Choose only Thunderbird's ordinary prefs.js file.
accounts-import-too-large = These account preferences exceed the import limit.
accounts-import-malformed = These account preferences are incomplete or malformed.
accounts-import-unsupported = This account can’t be set up automatically. Enter its server details yourself.
accounts-import-duplicate = Two selected accounts use the same address. Choose one configuration.
accounts-import-chooser-unavailable = The file chooser is available only in an interactive Settings window. A granted preferences path can be discovered through the account action.
accounts-import-plain = Unencrypted
accounts-import-starttls = STARTTLS
accounts-import-tls = TLS

accounts-browse-files = Browse files
accounts-choose-import-files = Choose files to import…
accounts-open-files-description = Open this connected account in Files
accounts-recover-mode = Recover mode change
accounts-recover-mode-description = An interrupted mode change keeps Sync stopped to protect your edits. Close the connection form, then recover the mode change before retrying.
accounts-files-configured-sync = Files · Keep files in sync
accounts-files-configured-import = Import only
accounts-add-provider = Add { $kind }
accounts-status-import-only = Import only
accounts-import-no-sync = No ongoing sync
accounts-status-mail-granted = Connected, not synced yet
accounts-mail-not-synced = Mail access granted. No first sync yet.
accounts-remove-files = Remove Files
accounts-disconnect-mail = Disconnect Mail
accounts-service-action = { $action } — { $service }

## Appearance: desk widgets

settings-desk-card = Desk widgets
settings-desk-music = Music
settings-desk-team = People in this workspace
settings-desk-calendar = Calendar
settings-desk-system-monitor = System monitor
settings-desk-when = Shows
settings-desk-edge = Waits at
settings-desk-show-pause = When I pause typing
settings-desk-show-always = Always, on the strips I pin it to
settings-desk-show-ask = Only when I ask
settings-desk-edge-left = Left edge
settings-desk-edge-right = Right edge
settings-desk-edge-bottom = Bottom edge
settings-desk-edge-bottom-left = Bottom-left corner
settings-desk-edge-bottom-right = Bottom-right corner
settings-desk-note = Widgets step aside while you type and come back when you pause or reach for their edge. Hold or tap Super to see them all. Drag one to move it to another edge; its pin keeps it on the strip you are on.

settings-drives-title = Remembered drives
settings-drives-intro = Drives you chose to remember open without asking again. A drive inside this computer opens when you open it in Files; an external one is still asked about each time you connect it.
settings-drives-unnamed-internal = Drive in this computer
settings-drives-unnamed-external = External drive
settings-drives-internal = In this computer
settings-drives-external = External
settings-drives-open-now = Open now
settings-drives-external-detail = Opens safely. You're asked each time you connect it.
settings-drives-internal-fast-detail = Opens with the fastest driver.
settings-drives-internal-safely-detail = Opens safely, with a confined driver.
settings-drives-open-safely = Open { $drive } safely
settings-drives-open-safely-desc = A bit slower. If the system on this drive were compromised, a crafted file system reaches a sandbox instead of the kernel. Applies the next time it opens.
settings-drives-forget = Forget
settings-drives-forgotten = Forgotten. You'll be asked again the next time you open this drive.
settings-drives-now-safely = This drive opens safely from its next opening on.
settings-drives-now-fast = This drive opens with the fastest driver from its next opening on.
settings-drives-changing = Changing…
settings-drives-busy = A change to your drives is still running.
settings-drives-change-error = Not changed: { $error }
settings-drives-bad-request = That is not a remembered drive.
settings-drives-unexpected = The drive service gave an unexpected answer.
settings-drives-refreshed = Drives read again.

# Which shortcut pastes in an app when the shell pastes for you (Super+V)
input-paste-caption = When you choose an image or files in clipboard history (Super+V), Tilo pastes them with the app's own shortcut: Ctrl+Shift+V in terminals, Ctrl+V elsewhere. Set it here for an app that pastes differently.
input-paste-ctrl-v = Ctrl+V
input-paste-ctrl-shift-v = Ctrl+Shift+V

settings-row-more = More for { $name }
settings-change-not-saved = That change wasn’t saved, so the setting went back to what it was. Try again.
