# The arrival screen's lines (web/arrival.mjs): one at a time while the
# desktop loads, in random order, two jokes to a tip, none twice a visit.
# Each message is one line; keep lines under about 40 characters.
#   arrival-joke-*         a joke, everywhere
#   arrival-coffee-*       a joke shown only on tilo.coffee (and its
#                          subdomains), the hosted Lounge; delete these to
#                          drop them, nothing else names them
#   arrival-tip-*          a tip, on the desktop and on a full-tab app
#   arrival-desktop-tip-*  a tip about the desktop, on the desktop only
#   …-mac                  the same tip worded for a Mac
#   arrival-label-tip      the dim label before every tip
# A tip ships only once what it says works in the browser.

## Old computers
arrival-joke-netscape = Best viewed in Netscape Navigator 4.0 at 800×600
arrival-joke-splines = Reticulating splines…
arrival-joke-downloading-cloud = Downloading the cloud…
arrival-joke-more-ram = Downloading more RAM…
arrival-joke-zero-updates = Installing 0 updates. You’re welcome.
arrival-joke-do-not-turn-off = Please do not turn off your browser.
arrival-joke-clippy = Clippy is not coming back.
arrival-joke-new-computer = Getting your new computer ready…
arrival-joke-off-and-on = Have you tried turning it off and on again?

## Together
arrival-joke-teaching-apps = Teaching apps to behave…
arrival-joke-cursor-colours = Assigning cursor colours…
arrival-joke-merging-opinions = Merging everyone’s opinions…
arrival-joke-resolving-conflicts = Resolving conflicts amicably…
arrival-joke-strangers = Syncing with people you haven’t met yet…
arrival-joke-counting-cursors = Counting cursors…
arrival-joke-herding-agents = Herding agents…
arrival-joke-end-to-end = Encrypting end to end, and the middle too…
arrival-joke-catching-up = Catching up…

## The Lounge and coffee
arrival-coffee-brewing = Brewing tilo.coffee…
arrival-coffee-cushions = Fluffing the Lounge cushions…
arrival-joke-grinding-beans = Grinding beans…
arrival-joke-saving-a-seat = Saving you a seat…
arrival-joke-doughnuts = Eating doughnuts…

## About ourselves
arrival-joke-free-beer = Free as in free beer (and freedom, too)
arrival-joke-fomo = Fighting FOMO…
arrival-joke-taking-our-time = Taking our time…
arrival-joke-shy-gpu = Warming up the GPU (it’s shy)…

## Tips
# Shown dim before each tip; jokes have none.
arrival-label-tip = Tip
arrival-desktop-tip-find-app = Ctrl+K finds any app
arrival-desktop-tip-find-app-mac = ⌘K finds any app
arrival-desktop-tip-scroll-sideways = Scroll sideways to move between windows
arrival-tip-saved = Everything here is saved as you go
arrival-tip-operating-system = Tilo is an operating system. This one runs in your browser.
# Tips that wait for their features to work in the browser, added here once
# they do:
#   Click someone’s avatar to see what they’re up to
#   Wiggle your cursor next to someone to chat with them
#   Strips as places

## Said instead of a line when a step of getting in is slow
arrival-slow-loading = Still loading. Your network may be slow.
arrival-slow-connecting = Still connecting. Your network may be slow.
arrival-slow-starting = Still starting Tilo. This device may be busy.
arrival-slow-opening = Still opening the workspace.
arrival-slow-drawing = Still starting graphics. Without a graphics card this takes longer.
# Said with Try again beside it once a step has not moved on for long; the
# page keeps waiting meanwhile.
arrival-stalled = This is taking longer than it should. Keep waiting, or try again.

## Said in the line's place when the page cannot get in or go on
# { $workspace } is the workspace's name; a message ending in -unnamed is
# said where the page does not know it.
arrival-retry = Try again
arrival-failed-stopped-drawing = The desktop stopped working. Try again to start it.
arrival-failed-unreachable = Can’t reach { $workspace } right now. Check your connection, then try again.
arrival-failed-unreachable-unnamed = Can’t reach the workspace right now. Check your connection, then try again.
arrival-failed-desktop = You got into { $workspace }, but its desktop did not start in this tab. Try again.
arrival-failed-desktop-unnamed = You got into the workspace, but its desktop did not start in this tab. Try again.
arrival-failed-general = Something went wrong opening { $workspace }. Try again, or come back later.
arrival-failed-general-unnamed = Something went wrong opening the workspace. Try again, or come back later.
arrival-failed-removed = You were removed from { $workspace }. To come back, ask someone in it for a new invitation.
arrival-failed-removed-unnamed = You were removed from this workspace. To come back, ask someone in it for a new invitation.
arrival-failed-invitation-only = You need an invitation to join { $workspace } for now. Ask someone in it for an invite link, then open it here.
arrival-failed-invitation-only-unnamed = You need an invitation to join this workspace for now. Ask someone in it for an invite link, then open it here.
arrival-door-heading = { $workspace } is invite-only for now
arrival-door-heading-unnamed = This workspace is invite-only for now
arrival-door-body = Ask someone in it for an invite link, then paste it here.
arrival-invite-link = Invite link
arrival-invite-placeholder = Paste an invite link
arrival-invite-open = Open
arrival-invite-not-a-link = That isn’t an invite link. Paste the whole link you were sent.
arrival-door-wait-heading = Sorry, Tilo is invite-only right now
arrival-door-wait-body = It’s an early alpha. Leave your email and we’ll send you an invite when there’s room.
arrival-wait-email = Email
arrival-wait-placeholder = you@example.com
arrival-wait-join = Join the wait list
arrival-wait-done = You’re on the list. We’ll email you at { $email }.
arrival-wait-privacy = We only use your email to send you an invite.
arrival-wait-removal = To be taken off the list, write to { $contact }.
arrival-wait-invalid = That doesn’t look like an email address.
arrival-wait-too-many = Too many sign-ups from your network just now. Try again later.
arrival-wait-full = The wait list is full for now. Please try again in a few days.
arrival-wait-failed = That didn’t go through. Try again in a moment.
arrival-sign-in-ask = Already have a Tilo identity?
arrival-sign-in = Sign in
arrival-sign-in-not-member = You’re signed in, but this identity isn’t in { $workspace } yet. Join the wait list, or ask someone in it for an invite.
arrival-sign-in-not-member-unnamed = You’re signed in, but this identity isn’t in this workspace yet. Ask someone in it for an invite.
arrival-sign-in-unknown = That passkey doesn’t open a Tilo identity this site knows. Use one you made on Tilo, or open Tilo where you already use it.
arrival-sign-in-failed = Signing in didn’t work in this browser. Try again, or open Tilo where you already use it.
arrival-invite-toggle = Have an invite link?
arrival-door-own = Or try Tilo on your own
arrival-failed-cooling-down = { $workspace } is not letting anyone in from your network for now, because someone here was removed from it. Ask someone in it for an invitation, or try again tomorrow.
arrival-failed-cooling-down-unnamed = This workspace is not letting anyone in from your network for now, because someone here was removed from it. Ask someone in it for an invitation, or try again tomorrow.
arrival-failed-custody-busy = Every device this browser keeps for { $workspace } is open in another tab. Close one of those tabs, then try again.
arrival-failed-custody-busy-unnamed = Every device this browser keeps for the workspace is open in another tab. Close one of those tabs, then try again.

## The reconnect screen: the page was not let in yet and tries again
# { $time } is how long until the next try, such as "40 s" or "2 min 05 s".
arrival-reconnect-unreachable = Can’t reach { $workspace }
arrival-reconnect-unreachable-unnamed = Can’t reach the workspace
arrival-reconnect-unreachable-body = It may be restarting. It opens here as soon as it’s back.
arrival-reconnect-refused = Not in { $workspace } yet
arrival-reconnect-refused-unnamed = Not in the workspace yet
arrival-reconnect-refused-body = It turned this browser away for now. The next try asks as a new device of yours.
arrival-reconnect-offline = You’re offline
arrival-reconnect-offline-body = The workspace opens as soon as you’re back online.
arrival-reconnect-busy = { $workspace } is busy
arrival-reconnect-busy-unnamed = The workspace is busy
arrival-reconnect-busy-body = A lot of people are getting in right now. It opens here once there’s room.
arrival-reconnect-next = Trying again in { $time }
arrival-reconnect-trying = Trying now…
arrival-reconnect-waiting-online = Waiting for the network…
arrival-reconnect-try-now = Try now
arrival-reconnect-details = Technical details
arrival-reconnect-copy = Copy details
arrival-reconnect-copied = Copied
arrival-reconnect-copy-failed = Couldn’t copy. Select the text instead.
