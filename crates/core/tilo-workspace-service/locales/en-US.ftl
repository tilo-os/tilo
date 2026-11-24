tilo-servers-ask-operation = Ask a server to do something
tilo-servers-set-floor-key = Set its rollback floor key
tilo-servers-answer-operation = Keep what a server answered

## Actions

tilo-servers = Servers
tilo-servers-enroll-server = Add server
tilo-servers-plan-machine = Plan a machine that is not installed yet
tilo-servers-begin-install = Its machine enrolled and is installing
tilo-servers-answer-machine-choice = Answer a question its machine asked
tilo-servers-finish-install = Its machine finished installing
tilo-servers-rename-server = Rename server
tilo-servers-hand-planned-workspace = Hand a server a workspace its plan names
tilo-servers-set-keeping = Keep a workspace or not
tilo-servers-set-appetite = Set what it offers
tilo-servers-set-updates = Set the updates a server follows
tilo-servers-set-relay-url = Say where its packet relay is reached
tilo-servers-set-web = Set whether it serves its web UI
tilo-servers-set-maintenance-window = Set maintenance window
tilo-servers-set-unlock-policy = Set how it unlocks
tilo-servers-set-hostname = Set its hostname
tilo-servers-use-disk = Use a drive
tilo-servers-release-disk = Stop using a drive
tilo-servers-set-interface = Set up a network interface
tilo-servers-release-interface = Stop configuring a network interface
tilo-servers-report-device-config = Say what its machine applied
tilo-servers-retire-server = Retire server
tilo-region = Region and formats
tilo-region-set-formats = Set date, time and number formats
tilo-region-choose-zone = Choose a time zone
tilo-region-browser-zone = Keep the browser's time zone
tilo-servers-ask-retirement = Ask a server to retire
# The name a person's own workspace gets when they do not name it, read in
# their language when they found it.
workspace-default-name = My workspace

# Why a person's own model gave no answer to work done for them, said where
# they read it, with what they can do about it.
model-none-linked = No model is linked for this kind of work. Choose one in Settings, under Agent models.
model-choice-unusable = { $model } from { $provider } can no longer be used. Choose another model in Settings.
model-choice-gone = { $model } can no longer be used. Choose another model in Settings.
model-no-key = { $provider } has no API key here yet. Add it in Settings.
model-signed-out = { $provider } needs you to sign in again. Sign in to it in Settings, under Model providers.
model-key-refused = { $provider } did not accept your API key. Check the key, or use another key for { $provider }.
model-key-not-allowed = { $provider } does not let your API key do this. Check what the key may do at { $provider }, or use another key.
model-key-no-credit = The account of your { $provider } key has no credit left. Add credit at { $provider }, or use another key.
model-address-refused = { $provider } is at an address that cannot be reached from here.
model-settings-unreadable = Your model settings could not be read. Try again in a moment.
model-cannot-take-picture = { $model } does not read pictures. Choose a model that does in Settings.
model-workspace-refuses = This workspace does not let its content go to { $provider }.
model-day-limit = { $agent } has used today's { $limit }. Raise its limit to let it go on.
model-timed-out = The model didn't answer in time.
model-cancelled = The model call was stopped.
model-truncated = The model's answer was cut off.
model-refused = The model declined this request.
model-context-limit = The conversation is longer than the model can take. Choose a model with more room in Settings, under Agent models.
model-rate-limited = The model is busy right now. Try again in a moment.
model-unsupported = The model cannot do this. Choose another model in Settings, under Agent models.
model-nothing-to-work-with = No model is linked for this workspace's agents. Choose one in Settings, under Agent models.
tilo-devices = Your devices
tilo-devices-keep-device = Keep a device of the person's
tilo-devices-keep-fact = Keep what a device says of itself
tilo-devices-keep-wifi = Keep a Wi-Fi passphrase
tilo-devices-say-session = Say how your session on a device stands
tilo-devices-allow-unlock = Let a device unlock its disk
tilo-devices-refuse-unlock = Refuse a device unlocking its disk
tilo-devices-wake-device = Wake a device
tilo-devices-wake-sent = Say a device sent the signal that wakes another
tilo-devices-woke = Say a device is awake again
tilo-devices-name-device = Rename a device
tilo-devices-keep-agent = Keep the agent a device runs
tilo-devices-revoke-device = Mark a device taken back
tilo-devices-mark-root-holder = Mark the device that keeps the root

## Nearby Send

so-tilo-device-nearby-send-send = Send files to a nearby device
so-tilo-device-nearby-send-accept = Keep a file a nearby device offered
so-tilo-device-nearby-send-decline = Turn down a file a nearby device offered
so-tilo-device-nearby-send-cancel = Stop a transfer, sent or received
so-tilo-device-nearby-send-retry-transfer = Offer again a transfer that failed
so-tilo-device-nearby-send-share = Share a resource with people, opening the invitation sheet
# What a turn says it is doing, as the person reads it on its reply: one line per step, what is
# being done and what app it is about.
agent-step-searching = Searching the web for “{ $what }”…
agent-step-reading = Reading { $what }…
agent-step-editing = Editing { $what }…
agent-step-exploring = Looking at what { $what } can do…

## Receivers that are not the person's devices

airdrop-error-unavailable = AirDrop requires file-transfer permission and current Files access.
airdrop-error-source = AirDrop accepts one readable regular file up to 16 MiB.
airdrop-error-busy = Another AirDrop attempt is still running. Cancel it before starting another.
airdrop-error-failed = AirDrop transfer failed. Verify the receiver again before a new attempt.
airdrop-error-declined = AirDrop transfer was cancelled before sending.

device-agents-not-accepted = This device doesn’t accept agents from your other devices. Turn on “Accept agents from my other devices” in its Settings, under Devices.

admit-not-permitted = You can’t invite people to this workspace. Its owner or an administrator can let you add members.
admit-above-role = You can invite people at your own role or below, and { $role } is above yours.
invitation-issuer-admits-no-more = The person who invited you can no longer let people in at this role. Ask them for a new invitation.

task-programs-resource = Task programs
task-programs-run = Run a program
task-programs-run-detail = Runs a program as a step of the task, and answers what it read or changed.
tilo-membership = Membership
tilo-membership-keep-role = Keep a member's role
tilo-ownership = Ownership
tilo-ownership-relabel = Label the workspace
tilo-ownership-run-unattended = Let keys run commands in its machine unasked, or stop them
tilo-host-blocked = Blocked devices and people
tilo-host-blocked-block-device = Block a device
tilo-host-blocked-unblock-device = Stop blocking a device
tilo-host-blocked-block-user = Block a person and all their devices
tilo-host-blocked-unblock-user = Stop blocking a person

## Workspace vault
passwords-workspace-vault = Workspace vault
passwords-workspace-vault-keep-sealed = Save encrypted vault entry
passwords-workspace-vault-import-sealed = Import encrypted vault entry
passwords-workspace-vault-remove-sealed = Delete encrypted vault entry
passwords-workspace-vault-keep-uses = Record where an entry was used

## App Store
store-catalog = Store catalogue
store-report-create = Create report
store-catalog-record-moderation-case = Record moderation case
