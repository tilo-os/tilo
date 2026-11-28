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
tilo-servers-set-kept-bytes = Set how many bytes of kept content it keeps for each workspace
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

# Said in an app's window when the workspace refuses what the app wrote by itself
# (a trigger, an update of its records) because a member took back the
# permission installing it gave: it keeps nothing until that is given back.
app-records-taken-back = { $app } can't keep its records in this workspace any more, so it stopped: whoever installed it can't reach them now. Installing { $app } again lets it carry on.
app-records-not-installed = { $app } isn't installed in this workspace, so it can't keep records here. Install { $app } in the workspace to use it.

# Said where an app opens when the workspace runs no version of it and opening it could not
# add it there: adding apps to the workspace is up to its owners.
app-open-not-added = { $app } isn't in this workspace yet, and only its owners can add apps here. Ask an owner to add { $app }.

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
tilo-devices-ask-input = Ask to use another device with this keyboard and pointer
tilo-devices-grant-input = Let another device drive this one
tilo-devices-end-input = Stop using a device with another keyboard and pointer
tilo-devices-show-notice = Show a notification on your other devices
tilo-devices-answer-notice = Answer a notification shown from another device
tilo-devices-retire-notice = Take a notification off your other devices
tilo-devices-offer-continuity = Offer the app this device has in front to your other devices
tilo-devices-answer-continuity = Take an app offered from another device, or put it away
tilo-devices-show-window-on-device = Show a window on another of your devices
tilo-devices-woke = Say a device is awake again
tilo-devices-move-accessory = Move a Bluetooth accessory to another device
tilo-devices-accessory-ready = Say whether a device holds an accessory's pairing
tilo-devices-accessory-released = Say a device let go of an accessory
tilo-devices-accessory-taken = Say a device connected an accessory
tilo-devices-give-up-move = Give up moving an accessory
tilo-devices-name-device = Rename a device
tilo-devices-keep-agent = Keep the agent a device runs
tilo-devices-revoke-device = Mark a device taken back
tilo-devices-mark-root-holder = Mark the device that keeps the root
tilo-devices-say-speaker-endpoints = Say the outputs and microphones a device offers to speaker groups
tilo-devices-lead-speaker-group = Keep the group a device leads, with its speakers and their roles
tilo-devices-drop-speaker-group = Forget a group a device led
tilo-devices-consent-speaker-group = Say what a device agreed to of another device's group
tilo-devices-withdraw-speaker-group = Take back what a device agreed to of another device's group
tilo-devices-offer-speaker-playback = Offer a group's playback to one of its member devices
tilo-devices-admit-speaker-playback = Admit a group's playback on a member device
tilo-devices-refuse-speaker-playback = Refuse a group's playback on a member device
tilo-devices-stop-speaker-playback = End a group's playback
tilo-device-answers = Your answers about your devices
tilo-device-answers-answer = Keep what an app may use on a device
tilo-device-answers-take-back = Take back what an app may use on a device

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
tilo-accounts-connected = Connected accounts
tilo-accounts-connected-keep = Keep a connected account
tilo-accounts-connected-forget = Forget a connected account

## Workspace vault
passwords-workspace-vault = Workspace vault
passwords-workspace-vault-keep-sealed = Save encrypted vault entry
passwords-workspace-vault-import-sealed = Import encrypted vault entry
passwords-workspace-vault-remove-sealed = Delete encrypted vault entry
passwords-workspace-vault-keep-uses = Record where an entry was used
browser-environment = Workspace computer settings


tilo-servers-floor = Rollback floor
tilo-servers-floor-set-floor-key = Set the floor key
tilo-servers-floor-request-floor-advance = Ask to raise the floor
tilo-servers-floor-approve-floor-advance = Approve raising the floor
tilo-servers-floor-refuse-floor-advance = Refuse raising the floor

# What a person's assistant tells them: kept by the host that keeps the person's workspace.
tilo-telling-tell = Tell the person
tilo-telling-list = What was told
tilo-telling-acknowledge = Say a thing was seen
tilo-telling-insist = Mark as not to be missed
tilo-telling-since-you-left = What happened while they were away
founding-create-question = Make the workspace “{ $name }” that { $device } asked for?
# A workspace's governance, and the host keeping it, as they are listed.
tilo-workspace-invite = Invite
tilo-workspace-invitation-list = List invitations
tilo-workspace-invitation-cancel = Cancel invitation
tilo-workspace-member-add = Admit a person
tilo-workspace-remove-member = Remove member
tilo-workspace-role = Change role
tilo-workspace-remove = Remove member
tilo-workspace-ban = Ban
tilo-workspace-wait-list-list = List the wait list
tilo-workspace-wait-list-remove = Take an address off the wait list
tilo-workspace-wait-list-mail = Mail an invitation to an address on the wait list
tilo-workspace-server-list = List the servers keeping it
tilo-workspace-server-add = Have another server keep it
tilo-workspace-server-remove = Have a server keep it no more
tilo-workspace-server-move = Move it to another server
tilo-workspace-copies-set = Choose how many copies it keeps
tilo-workspace-content-held = Count the content still only on members' devices
tilo-workspace-content-policy = What members may post here
tilo-workspace-content-set-policy = Set what members may post here
tilo-workspace-content-refused = The content this server stopped keeping, and why
tilo-workspace-copies-read = List the devices keeping it
tilo-workspace-rename = Rename the workspace
tilo-workspace-member-name = Set your name
tilo-workspace-unban = Unban
tilo-workspace-call = Moderate a call
tilo-workspace-publish = Make public
tilo-workspace-unpublish = Make private
tilo-workspace-publication = Public listing
tilo-workspace-install-app = Run a version of an app
tilo-workspace-upload-app-file = Hand the workspace an app version's package
tilo-workspace-app-offers = The apps the registries offer
tilo-workspace-trust-list = List the attestors this workspace trusts
tilo-workspace-trust-revoked = List the publisher keys this workspace refuses
tilo-workspace-trust-add = Trust an attestor
tilo-workspace-trust-remove = Stop trusting an attestor
tilo-workspace-trust-revoke = Refuse a publisher
tilo-workspace-registry-list = List the registries this workspace fetches apps from
tilo-workspace-registry-add = Add a registry
tilo-workspace-registry-remove = Remove a registry
tilo-workspace-registry-enable = Read a registry
tilo-workspace-registry-disable = Stop reading a registry
tilo-workspace-registry-channel = Choose a registry's release channel
tilo-workspace-uninstall-app = Let everyone run their own version of an app
tilo-workspace-app-pin = Keep this version
tilo-workspace-app-unpin = Let the app be updated
tilo-workspace-app-earlier = List the versions this workspace ran before
tilo-workspace-key-revoke = Revoke a key
tilo-workspace-webhook-list = List the webhooks this key registered
tilo-workspace-webhook-create = Register a webhook
tilo-workspace-webhook-delete = Stop a webhook this key registered
tilo-workspace-workspace-fetch-grant = Grant your page the host's fetch proxy token
tilo-workspace-grant-revoke = Revoke a grant
tilo-workspace-reading-grant = Let read
tilo-workspace-reading-revoke = Stop reading
tilo-workspace-server-rename = Rename a server
tilo-workspace-server-keep = Choose whether a server keeps a workspace
tilo-workspace-server-retire = Take a server out of service
tilo-host-workspace-create = Create workspace
tilo-host-workspace-create-own = Create your own workspace
tilo-host-workspace-delete = Delete your workspace
tilo-host-workspace-branch-own = Branch your workspace
tilo-host-workspace-list = List workspaces
tilo-host-identity-revocations = List revoked devices
tilo-host-host-block = Block a device or a person on this host
tilo-host-host-unblock = Stop blocking a device or a person on this host
tilo-host-workspace-enter = Enter workspace
tilo-host-workspace-host = Serve a workspace another host hands over
tilo-host-workspace-unhost = Stop serving a workspace another host handed over
tilo-host-workspace-host-frames = Take in a workspace's frames from a device keeping it
tilo-host-machine-plan = Plan a machine
tilo-host-machine-list = List the machines you planned
tilo-host-machine-show = Show a machine you planned
tilo-host-machine-servers = List your servers
tilo-host-machine-admit = Take a planned machine in
tilo-host-server-admit = Take a server into your servers
tilo-host-machine-keep-recovery-key = Keep this machine's disk recovery key
tilo-host-machine-recovery-keys = Read your machines' disk recovery keys
tilo-host-web-serve = Serve this server's web UI
