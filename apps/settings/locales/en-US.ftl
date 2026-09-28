# Personal Settings — user-facing strings.
#
# Panes move onto keys as they are rewritten; a string still a literal in the
# source is not translated yet, not intentionally excluded.

## AI pane
ai-pane-title = AI
ai-intro-title = Private by design
ai-intro-body = Models configured here supply inference only. They never grant an agent access to a workspace, app, file, message or device.
ai-intro-credentials = Keys and sign-ins
ai-intro-credentials-value = Held in the Tilo vault on this device
ai-intro-downloads = Downloads
ai-intro-downloads-value = Only when you confirm, pinned and verified
ai-this-device = this device
ai-notice-title = Last change
ai-notice-accepted = Accepted by the local model service.
# { $error } is the service's own explanation.
ai-notice-failed = That did not work: { $error }
ai-notice-release-gone = That model is no longer offered for this device.
ai-notice-signed-in = Signed in with Microsoft. Keys are no longer needed for this provider.
ai-gib = { $value } GiB
ai-gib-text = { $value } GiB

## Presets
ai-presets-title = Presets
ai-presets-body = A preset picks a model for every task from what this device can run and the providers you set up. A choice you make for a task or an agent always wins.
ai-preset-private = Private
ai-preset-private-description = Everything runs on this device. Nothing is sent to a provider.
ai-preset-balanced = Balanced
ai-preset-balanced-description = Private data stays on this device. Other tasks run here when this device has a strong graphics card, otherwise on your default provider.
ai-preset-best-quality = Best quality
ai-preset-best-quality-description = Tasks that may leave the device use your default provider at high reasoning effort. Private data still stays here.
ai-preset-use = Use
ai-preset-in-use = In use
ai-preset-none = No preset
# { $task } is a task name, { $model } a model, { $provider } where it runs.
ai-preset-outcome = { $task }: { $model } on { $provider }
ai-preset-outcome-missing = { $task }: { $reason }
ai-action-use-preset = Use the { $preset } preset
ai-action-preset-none = Stop using a preset

## Tasks
ai-tasks-title = Tasks
ai-tasks-body = Each feature asks for a task, never a model. Pick which model does each one; the gateway keeps every task within its privacy rules whatever is chosen.
ai-task-agent-planning = Agent planning
ai-task-agent-planning-description = An agent choosing which actions to take in your apps.
ai-task-agent-transform = Text transforms
ai-task-agent-transform-description = Summarising, extracting or classifying the selected document content an agent or shortcut hands it.
ai-task-photos-vision = Photo descriptions
ai-task-photos-vision-description = Captions, labels and the text in your photos, for search in Photos.
ai-task-photos-faces = Face groups
ai-task-photos-faces-description = Grouping your photos by the faces in them.
ai-task-feedback-review = Feedback review
ai-task-feedback-review-description = Suggesting what to hide and how to title each step before you send a feedback recording.
ai-privacy-open = Any provider
ai-privacy-sensitive = Stays here unless you allow
ai-privacy-device-only = This device only
ai-status-ready = Ready
ai-status-attention = Needs setup
# { $layer } says where the choice came from.
ai-resolved = { $model } on { $provider } · { $layer }
ai-layer-agent = chosen for this agent
ai-layer-task = chosen for this task
ai-layer-preset = { $preset } preset
ai-layer-recommended = recommended
ai-choose-label = Model
ai-choose-agent-label = Model for this agent
ai-choice-default = Recommended or preset
ai-choice-follow-task = Same as the task
ai-choice-target = { $model } · { $provider }
ai-choice-allowed-provider = A provider you allowed
ai-allow-off-device-button = Allow { $model } on { $provider }…
ai-action-choose-model = Choose the model for this task
ai-action-choose-agent-model = Choose the model for this agent
ai-action-allow-off-device = Allow { $task } to use a provider
ai-action-allow-off-device-description = This task works on private data. Allowing a provider sends that data to it. You can return the task to this device at any time.
ai-action-fast-mode = Use fast mode
ai-action-reasoning-effort = Set the reasoning effort
# { $configuration } names the model, its engine and the device, as ai-configuration.
ai-action-set-up-for-task = Set up { $configuration } for { $task }
ai-effort-default = Model default
ai-effort-minimal = Minimal effort
ai-effort-low = Low effort
ai-effort-medium = Medium effort
ai-effort-high = High effort
ai-effort-xhigh = Extra-high effort
ai-effort-max = Maximum effort
ai-feature-fast-mode = Fast mode
ai-feature-reasoning = Reasoning effort
ai-feature-tools = Tool use
ai-capability-vision = Images
ai-capability-face-recognition = Face groups
ai-capability-trace-review = Feedback review
ai-capability-text = Text
ai-good-for = Good for: { $list }
ai-error-no-model = No model is set up for this yet.
ai-error-no-local-model = No model on this device can do this yet.
ai-error-off-device = This task may only run on a model on this device.
ai-error-provider-unavailable = The chosen provider was removed or is turned off.
ai-error-model-unavailable = { $model } is not available right now.
ai-error-unmet = The chosen model cannot do this task.
ai-error-unpinned = The local model has no verified revision.
ai-error-options = The chosen model does not accept those options.

## Agents
ai-agents-title = Agents
ai-agents-body = Give an agent its own model. It still runs through your gateway and your keys, and it still needs its workspace permissions.
ai-agents-empty = No agents are members of your workspaces yet.
# { $agent } is the agent's name, { $task } a task name.
ai-agent-task = { $agent } · { $task }
ai-agent-pill = Agent

## Providers
ai-providers-title = Providers
ai-providers-body = Providers follow you to your other devices. Their keys stay in this device's vault, so another device asks for its own. A server you run on this computer, such as Ollama or LM Studio, stays with this computer; Tilo does not isolate it, so private tasks treat it like a provider.
ai-providers-empty = No providers yet. Add one, or use only models on this device.
ai-action-add-anthropic = Add Anthropic
ai-action-add-openai = Add OpenAI-compatible
ai-action-add-azure = Add Azure OpenAI
ai-action-add-private = Add a server on your network
ai-action-add-local-server = Add a server on this computer
ai-protocol-anthropic = Anthropic
ai-protocol-openai = OpenAI-compatible
ai-protocol-azure = Azure OpenAI
ai-location-hosted = hosted
ai-location-device = this device
ai-location-private = your network
ai-location-local-server = on this computer, not isolated by Tilo
# { $protocol } is the API dialect, { $location } where it runs.
ai-provider-kind = { $protocol } · { $location }
ai-provider-default = Default
ai-provider-name-placeholder = Provider name
ai-provider-endpoint-placeholder = https://api.example.com/v1
ai-provider-azure-endpoint-placeholder = https://your-resource.openai.azure.com/openai/v1
ai-provider-save = Save
ai-provider-credential = Sign-in
ai-provider-signed-in = Signed in with Microsoft
ai-provider-signed-out = Signed out: the Microsoft account was removed
ai-provider-key-placeholder = API key, stored in the Tilo vault
ai-provider-sign-in-unavailable = Microsoft sign-in is not configured in this installation. An API key works instead.
ai-provider-model-placeholder = Model id
ai-provider-deployment-placeholder = Deployment name
ai-provider-context-placeholder = Context window (tokens)
ai-provider-issue = Check this provider: { $issue }
ai-model-context = { $tokens } tokens
ai-model-context-unknown = Context unknown
ai-action-enable-provider = Turn on
ai-action-disable-provider = Turn off
ai-action-default-provider = Make default
ai-action-remove-provider = Remove…
ai-action-provider-details = Update the provider's name and endpoint
ai-action-set-key = Save key…
ai-action-remove-key = Remove key…
ai-action-sign-in-microsoft = Sign in with Microsoft…
ai-action-sign-out = Sign out
ai-action-sign-in-again = Sign in again…
ai-action-add-model = Add model
ai-action-add-deployment = Add deployment
ai-action-remove-model = Remove
ai-action-declare = Declares { $feature }
ai-health-connected = Connected
ai-health-needs-key = Needs a key
ai-health-signed-out = Signed out
ai-health-signing-in = Signing in…
ai-health-refused = Key refused
ai-health-failed = Error
ai-health-unreachable = Unreachable
ai-health-checking = Checking…
ai-health-unknown = Not checked yet
ai-health-disabled = Off

## This device
ai-device-title = This device
ai-device-unavailable = The local model service is unavailable, so models on this device cannot be shown or set up. Providers still work.
ai-device-unavailable-reason = Reason: { $reason }
ai-device-memory = Memory
ai-device-accelerator = Graphics
ai-device-storage = Free storage
# One GPU: { $memory } in GiB, { $driver } its kernel driver and version.
ai-gpu-dedicated = { $memory } GiB graphics memory · { $driver }
ai-gpu-integrated = Integrated · shares up to { $memory } GiB of memory · { $driver }
ai-gpu-no-driver = no driver
ai-gpu-none = No graphics card found
# { $engine } is an inference engine (vLLM, llama.cpp), { $device } a GPU or ai-device-cpu.
ai-configuration = { $model } with { $engine } on { $device }
ai-device-cpu = the processor
ai-recommended-title = Recommended for this device
ai-set-up-question = Set this up? { $configuration }
ai-set-up-yes = Set it up…
ai-show-more = Show more ({ $count })
ai-show-fewer = Show fewer
ai-alternatives-title = Other ways to run it on this device
ai-set-up-instead = Set this up instead…
ai-memory-dedicated = Needs { $need } of { $budget } GiB graphics memory
ai-memory-shared = Needs { $need } GiB of the memory the desktop also uses, { $budget } GiB to spare
ai-installed-runs = Runs with { $engine } on { $device }
ai-fit-excellent = Excellent fit
ai-fit-good = Good fit
ai-fit-tight = Runs, with limits
ai-fit-restart = Restart needed
ai-fit-unsupported = Not enough memory
ai-restart-pill = Restart
# { $vendor } is the graphics card's maker.
ai-restart-required = Restart to finish setting up your { $vendor } graphics driver
ai-restart-mismatch = The installed driver and the one running differ in version. Models wait for the card instead of running slowly without it.
ai-restart-staged = The driver is ready for the next start. Models wait for the card instead of running slowly without it.
ai-accelerator-title = Graphics driver
ai-accelerator-available = Ready to set up
ai-accelerator-available-detail = Tilo found a signed driver for this card. Setting it up prepares the next system image; nothing changes in the running system.
ai-accelerator-queued = Queued
ai-accelerator-queued-detail = You can leave Settings while Tilo verifies and prepares the driver.
ai-accelerator-staging = Preparing
ai-accelerator-staging-detail = Tilo is building the next system image. This session keeps running normally.
ai-accelerator-restart = Restart needed
ai-accelerator-restart-detail = The driver is ready for the next start. Restart from the power menu when convenient.
ai-accelerator-active = Active
ai-accelerator-active-detail = The signed driver is active and models on this device use the card.
ai-accelerator-error = Needs attention
ai-action-stage-accelerator = Set up the graphics driver…
ai-releases-title = Available for this device
ai-releases-none-fit = No signed model fits this device's memory and storage right now.
ai-releases-all-installed = Every model that fits this device is installed.
# { $speed } is how fast the model answers here.
ai-download-terms = { $download } GiB download, { $disk } GiB on disk · License { $license } ({ $url }) · Runs on this device · { $memory } · { $speed }
ai-speed-interactive = Fast here, about { $words } words a second
# { $seconds } is how long a typical answer (a feedback review) takes here.
ai-speed-slow = Slow here, about { $seconds } seconds an answer
ai-speed-impractical = Too slow for this device
ai-action-download = Set up { $configuration }…
ai-installed-title = Installed
ai-installed-empty = No models are installed. Each download shows its size and license before anything is fetched.
ai-phase-downloading = Downloading
ai-phase-installing = Installing
ai-phase-running = Running
ai-phase-ready = Stopped
ai-phase-error = Needs attention
ai-phase-downloading-detail = { $done } of { $total } GiB · safe to leave this page
ai-phase-running-detail = Serving tasks through the Tilo gateway on this device.
ai-phase-ready-detail = Installed and verified. Start it when you want tasks to use it; a stopped model uses no graphics memory.
ai-progress-label = Setting up { $model }
ai-local-vram = Graphics memory in use
ai-local-storage = On disk
ai-local-license = License
ai-action-start = Start
ai-action-stop = Stop
ai-action-retry = Retry
ai-action-remove-local = Remove…

# Displays pane: arrangement canvas
display-arrangement-title = Arrangement
display-arrangement-name = Display arrangement
display-arrangement-description = Drag a display to place it beside another. Each display's position menu does the same from the keyboard.
display-arrangement-empty = No displays to arrange
display-arrangement-item = Display { $number }: { $name }
display-arrangement-item-primary = Display { $number }: { $name }, primary
display-arrangement-item-position = { $width } × { $height } at { $x }, { $y }
display-arrangement-hint = Drag displays to match how they sit on your desk. Changes are staged until you apply them.
display-identify = Identify
display-identify-row = Show each display's number on it

# Displays pane: layout tools trusted to change displays without asking
display-tools-title = Display tools
display-tools-caption = These programs rearrange your displays without asking you to keep each change.
display-tools-forget = Ask again
display-tools-forgotten = This tool's display changes will ask to be kept again.

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
input-pad-button = Button { $number }
input-pad-button-none = Do nothing
input-pad-button-show-map = Show what the buttons do
input-pad-no-buttons = This pad reports no buttons
input-pad-mode-button-description = Switches the rings, strips and buttons to their next mode
input-pad-ring = Ring { $number }
input-pad-ring-mode = Ring { $number } · Mode { $mode }
input-pad-strip = Strip { $number }
input-pad-strip-mode = Strip { $number } · Mode { $mode }
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
shortcut-name-search = Global search
shortcut-name-workspace-overview = Overview
shortcut-name-lock = Lock
shortcut-name-quick-settings = Quick Settings
shortcut-name-shortcut-help = Desktop help
shortcut-name-remark = Say something about this
shortcut-name-focus-previous-window = Focus previous window
shortcut-name-focus-next-window = Focus next window
shortcut-name-previous-tab = Previous tab
shortcut-name-next-tab = Next tab
shortcut-name-previous-strip = Previous strip
shortcut-name-next-strip = Next strip
shortcut-name-previous-workspace = Previous workspace
shortcut-name-next-workspace = Next workspace
shortcut-name-toggle-maximize = Maximize or restore window
shortcut-name-detach-tab = Move tab to new group
shortcut-name-attach-tab-left = Move tab into group on left
shortcut-name-attach-tab-right = Move tab into group on right
shortcut-name-move-group-left = Move group left
shortcut-name-move-group-right = Move group right
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
shortcut-category-system = System
shortcut-category-navigation = Navigation
shortcut-category-windows = Windows
shortcut-category-apps = Apps
shortcut-category-capture = Capture
shortcut-category-accessibility = Accessibility
settings-shortcut-unassigned = Unassigned
settings-shortcuts-vim = Vim navigation (H J K L)
settings-shortcuts-actions = Actions
settings-shortcuts-actions-caption = A saved shortcut’s keyboard trigger binds its chord here; change or remove it in the shortcut.
