# Continuing on this desktop what you were just doing on another device.
continuity-continue = Continue { $app } from { $device }
continuity-continue-subtitle = Focus the tab you were just using on { $device }
continuity-dismiss = Dismiss
continuity-dismiss-title = Dismiss “Continue { $app } from { $device }”
continuity-dismiss-subtitle = Hidden until you switch tabs on { $device } again
continuity-other-device = your other device

# Undoing a tab close.
tab-closed-toast = Closed { $title }
tab-closed-group-toast = Closed { $count } tabs
tab-closed-undo = Undo
tab-closed-undo-hint = Select to undo, or press { $chord } later
tab-closed-undo-hint-unbound = Select to undo

# Display layout sheet (Super+P)
display-sheet-title = Display
display-sheet-subtitle = Choose where your desktop appears. Super+P moves to the next choice.
display-sheet-a11y-description = Up and Down move between choices, Enter applies the highlighted one, Super+P moves to the next layout, Escape closes.
display-mode-extend = Extend
display-mode-extend-detail = One desktop across every display
display-mode-mirror = Mirror
display-mode-mirror-detail = The same picture on every display
display-mode-builtin-only = Only this screen
display-mode-builtin-only-detail = External displays turn off
display-mode-external-only = Only external
display-mode-external-only-detail = The built-in screen turns off
display-mode-current = Current
display-mode-unavailable-single = Connect another display to use this
display-mode-unavailable-builtin = This device has no built-in screen
display-mode-unavailable-mirror = These displays share no resolution
display-confirm-title = Keep these display settings?
display-confirm-countdown = Reverting in { $seconds } s
display-confirm-applying = Applying…
display-confirm-keep = Keep
display-confirm-revert = Revert
display-rejected = Couldn't apply this layout: { $reason }

# Keep or revert a change a display tool (kanshi, wlr-randr) made
display-confirm-tool-title = Keep the display settings { $tool } applied?
display-confirm-tool-unknown-title = Keep the display settings a tool applied?
display-confirm-always = Always keep changes from { $tool }
display-confirm-a11y-description = A display tool changed your screens. Enter keeps the change, Escape restores the previous layout.

# Wireless receivers, the lower section of the same sheet
wireless-heading = Wireless displays
wireless-share-audio = Share audio
wireless-stream = Stream
wireless-stream-auto = Auto
wireless-stream-fixed = Fixed
wireless-stream-fps = { $fps } fps
wireless-credential-label = Receiver PIN or password
wireless-credential-placeholder = PIN or receiver password
wireless-credential-hint = Leave blank unless the receiver asks. Enter connects.
wireless-scanning = Looking for displays…
wireless-scanning-detail = Keep the receiver awake and on the same network.
wireless-scan-failed = Couldn't scan
wireless-none = No displays found
wireless-none-detail = Try Refresh, or check Wi-Fi and the receiver's casting mode.
wireless-connected = Connected
wireless-connecting = Connecting…
wireless-connect = Connect
wireless-stop = Stop
wireless-wait = Wait
wireless-protocols = Miracast · Google Cast · AirPlay · FCast

# Identify displays: the card painted on each display
display-identify-builtin = Built-in display
display-identify-unnamed = Display { $number }

# The Alt+Tab window switcher.
window-switcher-title = Switch windows
window-switcher-description = Windows in this workspace, most recently used first. Hold Alt and press Tab or the arrow keys to move, release Alt to switch, or press Escape to stay.
window-switcher-app-title = Switch windows of this app
window-switcher-app-description = This app's windows in this workspace, most recently used first. Hold Alt and press ` to move, release Alt to switch, or press Escape to stay.
window-switcher-untitled = Untitled window
window-switcher-caption-paged = { $title } ({ $position } of { $count })

# Fixed shell chords, as the shortcut guide and the keyboard map name them
shortcut-screenshot = Screenshot
shortcut-switch-windows = Switch windows
shortcut-switch-windows-backwards = Switch windows backwards
shortcut-switch-app-windows = Switch windows of the app
shortcut-close-tab = Close tab
shortcut-change-workspace = Change workspace
shortcut-new-window = New window
shortcut-add-tab = Add tab here
shortcut-duplicate-tab = Duplicate tab
shortcut-open-terminal = Open terminal
shortcut-global-search = Global search
shortcut-lock = Lock
shortcut-workspace-wheel = Workspace wheel
shortcut-display-layout = Display layout
shortcut-capture-screenshot = Capture screenshot
shortcut-picture-in-picture = Picture in picture
shortcut-save-recent-replay = Save recent replay
shortcut-workspace-history = Workspace history
shortcut-explore-workspace-history = Explore workspace history
shortcut-guide = Keyboard guide
shortcut-stop-remote-input = Stop all remote input
shortcut-toggle-mouse-keys = Toggle mouse keys

# What an editable shortcut does, completing “Press a new shortcut to …”
shortcut-action-search = open search
shortcut-action-workspace-overview = open the workspace overview
shortcut-action-lock = lock the screen
shortcut-action-quick-settings = open Quick Settings
shortcut-action-shortcut-help = open this guide
shortcut-action-focus-previous-window = focus the previous window
shortcut-action-focus-next-window = focus the next window
shortcut-action-previous-tab = select the previous tab
shortcut-action-next-tab = select the next tab
shortcut-action-previous-strip = select the previous strip
shortcut-action-next-strip = select the next strip
shortcut-action-previous-workspace = select the previous workspace
shortcut-action-next-workspace = select the next workspace
shortcut-action-toggle-maximize = maximize or restore the window
shortcut-action-detach-tab = move the tab into a new group
shortcut-action-attach-tab-left = move the tab into the group on the left
shortcut-action-attach-tab-right = move the tab into the group on the right
shortcut-action-move-group-left = move the group left
shortcut-action-move-group-right = move the group right
shortcut-action-move-group-to-previous-display = move the group to the display on the left
shortcut-action-move-group-to-next-display = move the group to the display on the right
shortcut-action-resize-narrower = make the window narrower
shortcut-action-resize-wider = make the window wider
shortcut-action-reopen-closed-tab = reopen the tab you closed last
shortcut-action-focus-group-1 = focus the first window in the strip
shortcut-action-focus-group-2 = focus the second window in the strip
shortcut-action-focus-group-3 = focus the third window in the strip
shortcut-action-focus-group-4 = focus the fourth window in the strip
shortcut-action-focus-group-5 = focus the fifth window in the strip
shortcut-action-focus-group-6 = focus the sixth window in the strip
shortcut-action-focus-group-7 = focus the seventh window in the strip
shortcut-action-focus-group-8 = focus the eighth window in the strip
shortcut-action-focus-group-9 = focus the ninth window in the strip
shortcut-action-open-files = open Files
shortcut-action-open-terminal = open a terminal
shortcut-action-clipboard-history = open clipboard history
shortcut-action-notifications = open notifications
shortcut-action-toggle-fullscreen = make the window full screen or restore it
shortcut-action-toggle-magnifier = turn the magnifier on or off
shortcut-action-magnifier-zoom-in = magnify more
shortcut-action-magnifier-zoom-out = magnify less
shortcut-action-power-menu = open the power menu
shortcut-action-next-input-source = switch to the next keyboard layout

# Shortcut guide (F1)
guide-title = Desktop help
guide-reopen-hint = Press F1 anywhere to reopen this guide · Select a navigation keycap to change it
guide-done = Done
guide-press-new-shortcut = Press a new shortcut to { $action } · Esc cancels
guide-message-esc-cancels = { $message } · Esc cancels
guide-rebind-needs-modifier = Add Super, Ctrl, Alt or Shift to printable keys.
guide-rebind-in-use = Already used to { $action }. Press another shortcut.
guide-rebind-reserved = F1 and Print Screen are reserved by the desktop.
guide-rebind-unsupported = That shortcut is not supported. Press another shortcut.
guide-rebind-taken-by-action = Already used to run an action. Press another shortcut.
guide-rebind-save-failed = Could not save that shortcut. Press another shortcut.
guide-reset-in-use = A default is already used to { $action }. Change that shortcut first.
guide-reset-taken-by-action = A default now runs an action. Change that shortcut first.
guide-reset-invalid = Could not restore the default shortcuts.
guide-reset-save-failed = Could not restore the defaults.
guide-controller-intro = Move with the stick or D-pad. Hold { $guide } to arrange windows. In a fullscreen game only { $guide } chords reach the desktop.
guide-row-move-focus = Move focus
guide-row-select = Select
guide-row-back = Back
guide-row-app-actions = Actions for this app
guide-row-search = Search
guide-row-this-guide = This guide
guide-row-windows = Windows
guide-row-tabs = Tabs
guide-row-move-tab = Move tab
guide-row-strips = Strips
guide-row-workspaces = Workspaces
guide-row-move-group = Move group
guide-row-move-window = Move window
guide-row-resize = Resize
guide-row-quick-settings = Quick settings
guide-touch-intro = Gestures follow the desktop's horizontal windows and vertical workspaces.
guide-touch-focus-next-window = Focus next window
guide-touch-focus-previous-window = Focus previous window
guide-touch-previous-workspace = Previous workspace
guide-touch-next-workspace = Next workspace
guide-touch-open-overview = Open overview
guide-touch-close-overview = Close overview
guide-navigation-heading = EDIT NAVIGATION SHORTCUTS
guide-essentials-heading = DESKTOP ESSENTIALS
guide-group-windows = WINDOWS & WORKSPACES
guide-group-search = SEARCH & SYSTEM
guide-group-capture = CAPTURE & DISPLAY
guide-reopen-closed-tab = Reopen closed tab
guide-overview = Overview
guide-quick-settings = Quick Settings
guide-unassigned = Unassigned
guide-unassigned-inline = unassigned
guide-pointer = Pointer: click to focus · drag tabs or groups to reorder · drag an edge to resize
guide-your-shortcuts = Your shortcuts: { $shortcuts }
guide-vim-mode = Vim mode · h j k l
guide-a11y-name = Desktop keyboard and mouse guide
guide-a11y-description = Focus windows with { $window-back } or { $window-next }; tabs with { $tab-back } or { $tab-next }; strips with { $strip-back } or { $strip-next }; workspaces with { $workspace-back } or { $workspace-next }; move groups with { $group-left } or { $group-right }; resize with { $narrower } or { $wider }. Essential desktop actions: { $essentials }. Click a displayed navigation shortcut to change it; Escape cancels a pending change. A changed row gets a curved-arrow button that restores its defaults. With a mouse, click windows and tabs, drag tabs or empty tab-bar space to reorder, and drag window gaps to resize. Escape closes this guide.

# Devices and media, the Quick Settings drill-in
devices-media-title = Devices and media
devices-media-description = Local audio outputs and identity-certified Tilo devices. Remote handoffs are direct and encrypted when reachable. Use Up and Down, Page Up and Page Down, Home, or End to move through every destination.
devices-media-list = Media destinations

# Quick Settings
qs-title = Quick Settings
qs-a11y-description = Frequently used system controls
qs-back = Back
qs-on = On
qs-off = Off
qs-connected = Connected
qs-disconnected = Disconnected
qs-connecting = Connecting…
qs-not-connected = Not connected
qs-tile-settings = Settings
qs-tile-settings-description = Open device settings
qs-tile-privacy = Privacy
qs-tile-privacy-description = App grants and enforced access for this workspace
qs-tile-history = History
qs-tile-history-description = Notification history
qs-tile-power = Power
qs-tile-power-description = Lock, log out, restart or shut down
qs-volume = Volume
qs-brightness = Brightness
qs-brightness-display = Brightness, { $display }
qs-slider-a11y-description = Use Left and Right Arrow to adjust
qs-network = Network
qs-wired-network = Wired network
qs-cable-unplugged = Cable unplugged
qs-bluetooth = Bluetooth
qs-vpn = VPN
qs-vpn-no-profiles = No profiles
qs-night-light = Night Light
qs-airplane-mode = Airplane mode
qs-mobile-data = Mobile data
qs-do-not-disturb = Do Not Disturb
qs-do-not-disturb-on = Notifications held
qs-rotation-lock = Rotation lock
qs-rotation-locked = Locked
qs-rotation-unlocked = Rotates with device
qs-devices = Devices
qs-devices-audio-and-media = Audio and media
qs-devices-count = { $count ->
    [one] 1 device
   *[other] { $count } devices
}
qs-devices-a11y-description = Personal devices, audio outputs and media routes
qs-clipboard = Clipboard
qs-clipboard-empty = Empty
qs-clipboard-count = { $count ->
    [one] 1 item
   *[other] { $count } items
}
qs-clipboard-a11y-description = Private clipboard history
qs-power-saver = Power Saver
qs-power-balanced = Balanced
qs-power-performance = Performance
qs-power-profile-a11y = Change power profile
qs-power-battery = { $percent }% battery
qs-power-ac = AC power
qs-power-kept-awake-by-app = { $power } · { $app } is preventing sleep
qs-power-kept-awake-by-apps = { $power } · { $count } apps are preventing sleep

# Quick Settings: editing the control grid
qs-edit-controls = Edit controls
qs-edit-help = Drag to reorder · Ctrl + Up/Down
qs-edit-a11y-description = Edit controls; drag or use Control plus Arrow keys to reorder
qs-edit-active-heading = ACTIVE CONTROLS
qs-edit-available-heading = AVAILABLE
qs-edit-reset = Reset defaults
qs-edit-reset-a11y = Reset Quick Settings defaults
qs-edit-done = Done
qs-edit-done-a11y = Finish editing Quick Settings
qs-edit-required-a11y = Required Quick Settings control
qs-edit-remove-a11y = Remove Quick Settings control
qs-edit-add-a11y = Add Quick Settings control
qs-edit-required-control-a11y = Required; use Control plus Arrow keys to reorder
qs-edit-optional-control-a11y = Select to add or remove; use Control plus Arrow keys to reorder

# Quick Settings: Wi-Fi
qs-wifi = Wi-Fi
qs-wifi-off = Wi-Fi is off
qs-wifi-scanning = Scanning for networks…
qs-wifi-scan-failed = Couldn’t scan · Refresh to try again
qs-wifi-none = No networks found
qs-wifi-choose-a11y = Choose a network, refresh the scan, or manage the Wi-Fi radio and hotspot
qs-wifi-rescan-a11y = Scan for Wi-Fi networks
qs-wifi-secured = Secured
qs-wifi-open = Open
qs-wifi-turn-on = Turn on Wi-Fi
qs-wifi-turn-off = Turn off Wi-Fi
qs-wifi-hotspot = Hotspot
qs-wifi-hotspot-a11y = Wi-Fi hotspot
qs-wifi-hotspot-sharing = Sharing this connection
qs-wifi-hotspot-turn-on = Turn on Wi-Fi hotspot
qs-wifi-hotspot-turn-off = Turn off Wi-Fi hotspot

# Quick Settings: Bluetooth
qs-bluetooth-empty = No devices found
qs-bluetooth-none-a11y = No paired Bluetooth devices found
qs-bluetooth-choose-a11y = Choose a paired Bluetooth device
qs-bluetooth-device-connected-a11y = Connected; open device actions
qs-bluetooth-device-paired-a11y = Paired; activate to connect

# Quick Settings: VPN
qs-vpn-empty = No VPN profiles yet · add one in Settings
qs-vpn-none-a11y = No VPN profiles configured
qs-vpn-choose-a11y = Connect or disconnect a configured VPN profile
qs-vpn-connected-a11y = VPN connected
qs-vpn-disconnected-a11y = VPN disconnected

# Quick Settings failures, as notifications
qs-action-failed = { $action } failed
qs-action-failed-refreshing = { $error }. The current device state is being refreshed.
qs-bluetooth-connect-failed = Could not connect { $device }
qs-bluetooth-disconnect-failed = Could not disconnect { $device }
qs-bluetooth-disconnected = Disconnected { $device } from this device
qs-vpn-change-failed = Could not change { $profile }

# Portal and permission sheets: app choice, global shortcuts, reviews, administrator access, network secrets, app-action grants
portal-choose-app-title = Choose an application
portal-chooser-this-item = this item
portal-chooser-detail = { $app } wants to open { $subject }
portal-open = Open
portal-cancel = Cancel
portal-allow = Allow
portal-authenticate = Authenticate
portal-connect = Connect
portal-shortcuts-title = Set global shortcuts
portal-shortcuts-detail = Select a row, then press the shortcut. Tilo and active app shortcuts cannot be claimed. Backspace clears a row.
portal-save-shortcuts = Save shortcuts
portal-shortcut-not-set = Not set
portal-shortcut-needs-modifier = Hold Ctrl, Alt, Shift, or Super, then press a key.
portal-shortcut-reserved = Reserved by Tilo. Choose another shortcut.
portal-shortcut-duplicate = Already assigned in this request. Choose another shortcut.
portal-shortcut-taken = Already used by { $app }. Choose another shortcut.
portal-shortcut-another-app = another application
portal-auth-heading = Administrator access
portal-auth-ended = Authentication ended unexpectedly.
portal-auth-rejected = That password was not accepted.
portal-grant-title = { $app } wants to use { $target }
portal-grant-detail-confirms = { $risk }
    It still asks you to confirm each time it runs.
portal-grant-detail-unattended = { $risk }
    It runs without asking you again.
portal-grant-once = Allow once
portal-grant-session = Allow for this session
portal-grant-always = Always allow in this workspace
portal-grant-deny = Deny
portal-risk-none = Only shows information
portal-risk-low = Low risk: an ordinary, reversible change
portal-risk-destructive = Destructive: can delete or overwrite
portal-risk-privileged = Privileged: crosses a security boundary
portal-risk-sensitive = Sensitive: reads or exports private data
portal-usb-title = Choose USB devices
portal-usb-reason = Select devices individually; read-write can modify a device.
portal-usb-resource = Space or click toggles a row. Access ends on release or disconnect.
portal-usb-confirm = Allow selected
portal-usb-unnamed-device = USB device
portal-usb-row-read-write = { $device } — Read and write
portal-usb-row-read-only = { $device } — Read only
portal-account-title = Share your identity?
portal-account-reason = The app is asking for your name and account identifier.
portal-account-resource = Your selected identity for this workspace
portal-account-confirm = Share identity
portal-launcher-web-app-title = Install this web app?
portal-launcher-title = Add this launcher?
portal-launcher-reason = Review the launcher before it is added to your application library. The supplied image is checked but never opened from an app path or file descriptor.
portal-launcher-confirm = Add launcher
portal-launcher-icon-supplied = Supplied image (validated)
portal-launcher-icon-monogram = Tilo monogram
portal-launcher-icon-compass = Tilo web compass
portal-background-title = Allow background activity?
portal-background-resource = The app may keep working after its window is closed.
portal-background-confirm = Allow in background
portal-wallpaper-title = Set workspace wallpaper?
portal-wallpaper-preview = A preview will be shown before this private workspace setting is applied.
portal-wallpaper-private = This changes only this device's private workspace backdrop.
portal-wallpaper-resource = Apply to { $target }
    Private imported image
portal-wallpaper-confirm = Set wallpaper
portal-email-title = Open your mail app?
portal-email-reason = Review the prefilled message before it is handed to your configured mail provider.
portal-email-confirm = Open mail app
portal-email-cc = Cc: { $address }
portal-email-bcc = Bcc: { $address }
portal-email-no-recipient = No recipient prefilled
portal-email-subject = Subject: { $subject }
portal-email-attachments = { $count ->
    [one] 1 attachment will be available through the document portal.
   *[other] { $count } attachments will be available through the document portal.
}
portal-remember-device = Remember on this device
portal-secret-required = Fill in every required field.
portal-secret-wifi-length = A Wi-Fi password is 8 to 63 characters, or 64 hexadecimal digits.
portal-network-heading = Network
portal-network-this-network = this network
portal-network-password = Password
portal-network-key = Key
portal-network-user-name = User name
portal-network-private-key-password = Private key password
portal-network-anonymous-identity = Anonymous identity (optional)
portal-network-wifi-title = Wi-Fi password required
portal-network-enter-password = Enter the password for “{ $network }”.
portal-network-wired-sign-in-title = Wired network sign-in
portal-network-sign-in-title = Network sign-in
portal-network-sign-in-lead = “{ $network }” needs your user name and password.
portal-network-vpn-title = VPN sign-in
portal-network-vpn-lead = Enter the secrets for “{ $network }”.
portal-network-sim-pin-title = SIM PIN required
portal-network-sim-pin-lead = Enter the PIN to unlock the SIM for “{ $network }”.
portal-network-sim-pin = SIM PIN
portal-network-mobile-title = Mobile broadband password required
portal-network-dsl-title = DSL password required
portal-network-retry = The last attempt did not work. Check it and try again.
portal-network-wps = You can also press the WPS button on the router.
portal-network-cert-pinned = The network's certificate is checked against this connection's certificate authority.
portal-network-cert-pinned-domain = The network's certificate is checked against this connection's certificate authority. It must belong to { $domain }.
portal-network-cert-system = The network's certificate is checked against the system's trusted authorities.
portal-network-cert-system-domain = The network's certificate is checked against the system's trusted authorities. It must belong to { $domain }.
portal-network-cert-unchecked = The network's certificate is not checked. Only continue on a network you trust.

# Top bar
topbar-workspace-name-tooltip = Open workspace settings
topbar-workspace-fallback = Workspace
topbar-calendar-none = No calendar connected
topbar-sharing-agent-controls = AGENT CONTROLS · { $name }
topbar-sharing-agent-viewing = AGENT VIEWING · { $name }
topbar-sharing-person-controls = CONTROLS · { $name }
topbar-sharing-person-viewing = VIEWING · { $name }
topbar-sharing-session-controls = SESSION CONTROLS · { $name }
topbar-sharing-session-viewing = SESSION VIEWING · { $name }
topbar-sharing-count-controlling = { $controllers } CONTROL · { $viewers } VIEWING
topbar-sharing-count = { $viewers } VIEWING
topbar-sharing-surface = surface { $surface }
topbar-open-overview = Open overview
topbar-previous-workspace = Previous workspace
topbar-next-workspace = Next workspace
topbar-workspace-settings = Workspace settings for { $workspace }
topbar-show-clock = Show clock
topbar-show-system-monitor = Show system monitor
topbar-show-calendar = Show calendar
topbar-open-quick-settings = Open quick settings
topbar-show-collaborators = Show collaborators
topbar-viewer-can-control = can control { $targets }
topbar-viewer-view-only = view only { $targets }
topbar-viewer-entry = { $name } ({ $kind ->
        [agent] agent
        [person] person
       *[session] unidentified session
    }): { $access }
topbar-collaborators-visible = Screen visible to { $viewers }. Show collaborators
topbar-capture-privacy = Privacy settings for active capture
topbar-control = Top bar control
topbar-editor-control = Top bar editor control
topbar-editor-remove-clock = Remove clock from top bar
topbar-editor-remove-system-monitor = Remove system monitor from top bar
topbar-editor-remove-calendar = Remove calendar from top bar
topbar-editor-add-clock = Add clock to top bar
topbar-editor-add-system-monitor = Add system monitor to top bar
topbar-editor-add-calendar = Add calendar to top bar
topbar-editor-title = Edit top bar
topbar-editor-hint = Drag to reorder · click Remove
topbar-widget-clock = Clock
topbar-widget-system-monitor = System monitor
topbar-widget-calendar = Calendar
topbar-editor-remove = Remove
topbar-editor-add-clock-chip = + Clock
topbar-editor-add-monitor-chip = + Monitor
topbar-editor-add-calendar-chip = + Calendar
topbar-invite = Invite
topbar-clocks-notification-title = Clocks
topbar-clocks-notification-body = Time is up
topbar-activity-unread = { $headline }  ·  { $count } new
topbar-clock-resume = Resume
topbar-clock-pause = Pause
topbar-clock-lap = Lap
topbar-clock-add-minute = +1 min
topbar-clock-close-stopwatch = Close floating stopwatch
topbar-clock-close-timer = Close floating timer
topbar-clock-floating-stopwatch = Floating stopwatch
topbar-clock-floating-timer = Floating timer
topbar-clock-floating-description = Drag to reposition. With a keyboard, hold Alt and use the arrow keys; hold Shift for larger steps.
topbar-clock-stopwatch = Stopwatch
topbar-clock-timer = Timer
topbar-clock-paused = Paused
topbar-clock-running = Running
topbar-clock-record-lap = Record lap
topbar-clock-add-one-minute = Add one minute
topbar-workspace-position = Workspace { $position } of { $total }
topbar-desktop-navigation = Desktop navigation
topbar-display-navigation = Display navigation
