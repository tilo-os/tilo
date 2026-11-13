# The arrival screen's lines (web/arrival.mjs): one at a time while the
# desktop loads, in random order, two jokes to a tip, none twice a visit.
# Each message is one line; keep lines under about 40 characters.
#   arrival-joke-*         a joke, everywhere
#   arrival-site-*         a joke shown only on tilo.sh (and its
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
arrival-joke-fax = Warming up the fax machine…

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
arrival-joke-form-a38 = Applying for form A38…

## The Lounge
arrival-site-cushions = Fluffing the Lounge cushions…
arrival-joke-grinding-beans = Grinding beans…
arrival-joke-saving-a-seat = Saving you a seat…
arrival-joke-doughnuts = Eating doughnuts…

## About ourselves
arrival-joke-free-beer = Free as in free beer (and freedom, too)
arrival-joke-fomo = Fighting FOMO…
arrival-joke-taking-our-time = Taking our time…
arrival-joke-shy-gpu = Warming up the GPU (it’s shy)…
arrival-joke-recycling = Sorting the recycling…

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
# The bar's line about the connection itself, kept for as long as it is true
# and cleared once it is not: a desktop stays mounted through a transport
# that dropped, an admission that ended, a store that stopped saving and a
# renderer that stopped drawing, so the mount is no answer to it.
arrival-failed-offline = You’re offline. Changes are kept on this device and sent once the connection is back.
arrival-failed-admission = The workspace can’t take this browser’s changes right now. Changes are kept on this device.
arrival-failed-saving = Your changes aren’t saved on this device yet. Keep this tab open and try again.
# A private window, or one where site data is turned off: the browser hands the
# page no file store, so a desktop draws here but its apps keep no files.
arrival-failed-files = This window keeps no files, so apps here can’t store files of their own. Open Tilo in a normal window to work with them.
arrival-failed-renderer = The desktop stopped drawing. Try again to start it, or reload the page.
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
arrival-wait-inbox = Check your inbox: we sent a link to { $email } to confirm.
arrival-wait-again-prompt = Didn’t get it?
arrival-wait-again = Send it again
arrival-wait-again-sent = Sent again. We send at most one link an hour, so check your spam folder too.
arrival-wait-listed = You’re on the list ({ $email })
arrival-wait-confirmed = You’re on the list.
arrival-wait-invited = You’re invited: the link is in your inbox.
arrival-wait-invited-no-mail = You’re invited: whoever invited you sends you the link.
arrival-wait-expired = That link no longer works. Leave your address again.
arrival-wait-change = Changed your mind?
arrival-wait-leave = Take me off the list
arrival-wait-left = You’re off the list.
arrival-wait-leave-too-many = Too many tries from your network just now. Try again later.
arrival-wait-privacy = We only use your email to send you an invite.
arrival-wait-privacy-mail = We email you a link to confirm, then your invite. Nothing else.
arrival-wait-no-mail = No email is sent from here: whoever invites you sends you the link.
arrival-wait-contact = Questions or removal: { $contact }
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

## The reconnect screen: the page was not let in yet and tries again
# { $time } is how long until the next try, such as "40 s" or "2 min 05 s".
arrival-reconnect-unreachable = Can’t reach { $workspace }
arrival-reconnect-unreachable-unnamed = Can’t reach the workspace
arrival-reconnect-unreachable-body = It may be restarting. It opens here as soon as it’s back.
arrival-reconnect-unreachable-local-body = { $host } may be restarting, or this browser doesn’t trust its certificate. Open it to see which; the workspace opens here as soon as it answers.
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

# The entry page's failure screen (failure-screen.mjs): a heading per case,
# named for the workspace where the page knows it, and a body that says the
# cause and what to do. `{ $host }` is the address of the host the page could
# not reach; `{ $step }` one of the entry-failure-step lines.
entry-failure-removed = You were removed from { $workspace }
entry-failure-removed-unnamed = You were removed from this workspace
entry-failure-removed-body = To come back, ask someone in it for a new invitation.
entry-failure-cooling-down = { $workspace } isn’t letting your network in
entry-failure-cooling-down-unnamed = This workspace isn’t letting your network in
entry-failure-cooling-down-body = Someone on your network was removed from it. Ask someone in it for an invitation, or try again tomorrow.
entry-failure-invitation-only = { $workspace } is invite-only for now
entry-failure-invitation-only-unnamed = This workspace is invite-only for now
entry-failure-invitation-only-body = Ask someone in it for an invite link, then open it here.
entry-failure-gone = { $workspace } no longer exists
entry-failure-gone-unnamed = This workspace no longer exists
entry-failure-gone-body = Its host was set up again since this browser last opened it.
entry-failure-deleted = { $workspace } was deleted
entry-failure-deleted-unnamed = This workspace was deleted
entry-failure-deleted-body = Its owner deleted it, so it can’t be opened anymore.
entry-failure-no-access = { $workspace } didn’t let this browser in
entry-failure-no-access-unnamed = This workspace didn’t let this browser in
entry-failure-no-access-body = You may have been removed from it, or it no longer exists.
entry-failure-refused = Couldn’t open { $workspace }
entry-failure-refused-unnamed = Couldn’t open this workspace
entry-failure-refused-body = { $reason }
entry-failure-custody-busy = { $workspace } is open in your other tabs
entry-failure-custody-busy-unnamed = This workspace is open in your other tabs
entry-failure-custody-busy-body = This browser can open it in only so many tabs at once. Close one of the others, then try again.
entry-failure-offline = You’re offline
entry-failure-offline-unnamed = You’re offline
entry-failure-offline-body = The workspace opens here as soon as you’re back online.
entry-failure-unreachable = Can’t connect to { $host }
entry-failure-unreachable-unnamed = Can’t connect to { $host }
entry-failure-unreachable-body = This site loads, so your connection works. The host may be restarting: try again in a moment.
entry-failure-unreachable-local = Can’t connect to { $host }
entry-failure-unreachable-local-unnamed = Can’t connect to { $host }
entry-failure-unreachable-local-body = This site loads, so your connection works. Either the host is down, or this browser doesn’t trust its certificate. Open { $host } to see which, then try again.
entry-failure-no-connection = Can’t connect to { $host }
entry-failure-no-connection-unnamed = Can’t connect to { $host }
entry-failure-no-connection-body = This site doesn’t answer either. Check your connection, then try again.
entry-failure-host-silent = { $host } didn’t let this tab into { $workspace }
entry-failure-host-silent-unnamed = { $host } didn’t let this tab in
entry-failure-host-silent-body = The host answered, but didn’t let this tab in. Try again. If it stops again, copy the technical details below and send them to whoever runs this site.
entry-failure-host-error = { $host } couldn’t open { $workspace }
entry-failure-host-error-unnamed = { $host } couldn’t open this workspace
entry-failure-host-error-body = It answered with an error. If it was restarting, trying again in a moment opens it.
entry-failure-desktop = { $workspace } didn’t start
entry-failure-desktop-unnamed = This workspace didn’t start
entry-failure-desktop-body = { $step } Your windows and documents are kept in this browser, so trying again picks up where you left off.
entry-failure-stopped = { $workspace } stopped working
entry-failure-stopped-unnamed = The desktop stopped working
entry-failure-stopped-body = Your windows and documents are kept in this browser, so trying again picks up where you left off.
entry-failure-loading = Couldn’t open { $workspace }
entry-failure-loading-unnamed = Couldn’t open Tilo
entry-failure-loading-body = { $step } Try again. If it stops again, copy the technical details below and send them to whoever runs this site.
entry-failure-step-load = It stopped while loading Tilo.
entry-failure-step-join = It stopped while joining.
entry-failure-step-open = It stopped while setting up its desktop.
entry-failure-step-draw = It stopped while drawing its first screen.
entry-failure-the-host = the host
entry-failure-refused-fallback = This link or invitation can’t be used.
entry-failure-open-host = Open { $host }
entry-failure-others = Or open another of your workspaces
