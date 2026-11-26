# What the arrival screen (web/arrival.mjs) says when a step of getting in
# runs slow or the page cannot go on. While the steps run, its line says
# which step it is, in the page's own words.

## Said instead of a line when a step of getting in is slow
arrival-slow-loading = Still loading Tilo.
arrival-slow-connecting = Still connecting to the workspace.
arrival-slow-starting = Still starting Tilo.
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
# The network is fine, but this browser holds no connection to the workspace:
# the workspace turned it away, or connecting again did not work.
arrival-failed-disconnected = This browser isn’t connected to the workspace. Changes are kept on this device. Reload the page to connect again.
arrival-failed-admission = The workspace can’t take this browser’s changes right now. Changes are kept on this device.
arrival-failed-saving = Your changes aren’t saved on this device yet. Keep this tab open and try again.
# A private window, or one where site data is turned off: the browser hands the
# page no file store, so a desktop draws here but its apps keep no files.
arrival-failed-files = This window keeps no files, so apps here can’t store files of their own. Open Tilo in a normal window to work with them.
arrival-failed-renderer = The desktop stopped drawing. Try again to start it, or reload the page.
arrival-refusal-heading = { $workspace } is invite-only for now
arrival-refusal-heading-unnamed = This workspace is invite-only for now
arrival-refusal-body = Ask someone in it for an invite link, then paste it here. If you are in it on another device, paste the link “Pair a new device” makes there.
arrival-refusal-sign-in-heading = Sign in to open { $workspace }
arrival-refusal-sign-in-heading-unnamed = Sign in to open this workspace
arrival-refusal-sign-in-body = Only the people in this workspace can open it. Sign in as one of them, or paste an invite link someone sent you.
arrival-refusal-sign-in-wait-body = Only the people in this workspace can open it. If you’re one of them, sign in. New to Tilo? It’s invite-only right now: leave your email and we’ll send you an invite.
arrival-invite-link = Invite or pairing link
arrival-invite-placeholder = Paste an invite or pairing link
arrival-invite-open = Open
arrival-invite-not-a-link = That isn’t an invite or pairing link. Paste the whole link.
arrival-refusal-wait-heading = One workspace for you, your team and your AI agents
arrival-refusal-wait-body = Tilo is invite-only for now. Leave your email and we’ll send you an invite.
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
arrival-sign-in-open = Sign in
arrival-sign-in-not-member = You’re signed in, but this identity isn’t in { $workspace } yet. Join the wait list, or ask someone in it for an invite.
arrival-sign-in-not-member-unnamed = You’re signed in, but this identity isn’t in this workspace yet. Ask someone in it for an invite.
arrival-invite-toggle = Have an invite link?

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
# cause and what to do. `{ $server }` is the address of the server the page could
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
entry-failure-gone-body = It was reset since this browser last opened it.
entry-failure-deleted = { $workspace } was deleted
entry-failure-deleted-unnamed = This workspace was deleted
entry-failure-deleted-body = Its owner deleted it, so it can’t be opened anymore.
entry-failure-no-access = { $workspace } didn’t let this browser in
entry-failure-no-access-unnamed = This workspace didn’t let this browser in
entry-failure-no-access-body = You’re not in it, or it no longer exists. Ask someone in it for an invite link.
entry-failure-refused = Couldn’t open { $workspace }
entry-failure-refused-unnamed = Couldn’t open this workspace
entry-failure-refused-body = { $reason }
entry-failure-device-key-busy = { $workspace } is open in your other tabs
entry-failure-device-key-busy-unnamed = This workspace is open in your other tabs
entry-failure-device-key-busy-body = This browser can open it in only so many tabs at once. Close one of the others, then try again.
entry-failure-offline = You’re offline
entry-failure-offline-unnamed = You’re offline
entry-failure-offline-body = The workspace opens here as soon as you’re back online.
entry-failure-unreachable = Can’t connect to { $server }
entry-failure-unreachable-unnamed = Can’t connect to { $server }
entry-failure-unreachable-body = This site loads, so your connection works. The host may be restarting: try again in a moment.
entry-failure-unreachable-local = Can’t connect to { $server }
entry-failure-unreachable-local-unnamed = Can’t connect to { $server }
entry-failure-unreachable-local-body = This site loads, so your connection works. Either the host is down, or this browser doesn’t trust its certificate. Open { $server } to see which, then try again.
entry-failure-no-connection = Can’t connect to { $server }
entry-failure-no-connection-unnamed = Can’t connect to { $server }
entry-failure-no-connection-body = This site doesn’t answer either. Check your connection, then try again.
entry-failure-host-refused = { $server } refused this tab entry to { $workspace }
entry-failure-host-refused-unnamed = { $server } refused this tab entry
entry-failure-host-refused-body = It said: { $reason }.
entry-failure-host-silent = { $server } didn’t let this tab into { $workspace }
entry-failure-host-silent-unnamed = { $server } didn’t let this tab in
entry-failure-host-silent-body = It answered, but kept this tab waiting. Try again in a moment.
entry-failure-certificate = { $server } didn’t accept this browser
entry-failure-certificate-unnamed = { $server } didn’t accept this browser
entry-failure-certificate-body = It turned down the certificate this browser signs in with.
entry-failure-incompatible = { $server } couldn’t read what this page sent
entry-failure-incompatible-unnamed = { $server } couldn’t read what this page sent
entry-failure-incompatible-body = This page and { $server } may be on different versions of Tilo. Try again to load the page anew.
entry-failure-join-stopped = { $server } stopped this tab joining { $workspace }
entry-failure-join-stopped-unnamed = { $server } stopped this tab joining
entry-failure-join-stopped-body = It said: { $reason } Try again.
entry-failure-host-error = { $server } couldn’t open { $workspace }
entry-failure-host-error-unnamed = { $server } couldn’t open this workspace
entry-failure-host-error-body = It answered with an error. If it was restarting, trying again in a moment opens it.
entry-failure-desktop = { $workspace } didn’t start
entry-failure-desktop-unnamed = This workspace didn’t start
entry-failure-desktop-body = { $step } Your windows and documents are kept in this browser, so trying again picks up where you left off.
entry-failure-stopped = { $workspace } stopped working
entry-failure-stopped-unnamed = The desktop stopped working
entry-failure-stopped-body = Your windows and documents are kept in this browser, so trying again picks up where you left off.
entry-failure-loading = Couldn’t open { $workspace }
entry-failure-loading-unnamed = Couldn’t open Tilo
entry-failure-loading-body = { $step } Try again.
entry-failure-step-load = It stopped while loading Tilo.
entry-failure-step-join = It stopped while joining the workspace.
entry-failure-step-open = It stopped while setting up its desktop.
entry-failure-step-draw = It stopped while drawing its first screen.
entry-failure-the-host = the host
entry-failure-refused-fallback = This link or invitation can’t be used.
entry-failure-open-host = Open { $server }
entry-failure-others = Or open another of your workspaces
entry-failure-elsewhere = Or open one of these instead

## The 24 recovery words, shown once right after a person is first let in
# recovery-words.mjs: the only way back into Tilo if the person loses every
# device. The words themselves are not translated.
recovery-words-heading = Your 24 recovery words
recovery-words-body = These 24 words are the only way back into Tilo if you lose every device. Write them down in order and keep them safe. Anyone who has them can be you.
recovery-words-copy = Copy
recovery-words-copied = Copied
recovery-words-confirm = I’ve written them down
