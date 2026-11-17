# Tilo Assistant for Mac and iPhone

Two apps from one Swift package: a native shell around Tilo's own page.

Only Tilo's web runtime draws a live document or a live view inside a conversation, so these
apps do not draw the conversation a second time. They show the page that a link to one app
opens ("Links opened full tab" in
[the web runtime's guide](../../crates/browser/tilo-browser-runtime/WEB_RUNTIME.md)) in the
system web view, and add what only the system can do. An iPhone has no other way to draw Tilo:
no native runtime exists for it.

```
Package.swift        the targets, and the oldest system the apps run on
Sources/ShellCore    decided without a web view: the site, which addresses stay inside, what
                     the page's title says, the hand-over from the share sheet, the sentences,
                     the script that joins page and system
Sources/ShellWeb     the web view that holds the page, notifications, dictation, the check
Sources/TiloMac      the Mac app: one window, the menu-bar item, settings
Sources/TiloPhone    the iPhone app
Sources/TiloShare    the share extension of both
Bundle/              what the bundles carry: Info.plists, entitlements, the English sentences,
                     the phone's page in Settings
build.sh             builds, signs, checks, installs
```

## What the system adds

**The page, kept.** The web view keeps the site's data. The page holds its device key as a
browser key that WebKit wraps with a key from the app's keychain, so a person who got in once
stays in for as long as the app keeps its signature and its data. Getting in is the site's own
flow (an invitation, a pairing link, its sign-in); nothing hands a native identity to a page.
While the site keeps a device out, a bar under the page offers the way in: paste the link, or
on a phone scan the code, that "Pair a new device" shows on a device where the person is
signed in. The page says where it stands at the door on its root element
(`data-entry`); the bar goes when it is in, and a person who came in by such a link is taken
back to the app. A link is also opened from the File menu on the Mac, from a long press on
the phone's icon, or by sharing it to the app.

**One site.** The app shows the origin its settings name and nothing else
(`SitePolicy.swift`). Any other address goes to the system's browser. The script below runs in
the page's main frame and does nothing on another origin, so no other site's content reaches
the system through it.

**Notifications.** A system web view has no working Notifications API, so the script
(`Bridge.swift`) is one for the page: `Notification` with `permission`, `requestPermission()`,
`close()` and the `click` and `close` events, as `crates/platform/tilo-notify/web/notify.mjs`
uses them. What the page shows becomes a notification of the system; pressing it brings the
app forward and hands the page its choice. A reply typed into a notification lands in the
message field. Beyond a browser, a page's notification may name `actions`
(`{action, title, type, placeholder}`, up to `Notification.maxActions`): each is a button in
the system's notification, a `type: 'text'` one a field, and the `click` event then carries
`action` and `reply`. On the Mac the page answers such a button without coming forward.

**The menu-bar item** (Mac). Closing the window leaves the app in the menu bar, and the page
goes on running behind it. The item shows the count the page writes into its title, with the
line the page wrote, and offers the window, dictation, settings and "Open at login". Opening
at login is the person's choice; nothing registers the app before they ask, and nothing of it
runs after Quit.

**Dictation.** Speech becomes words on the device (`Dictation.swift`); where the system
cannot recognise the language by itself, dictation is not offered. A panel shows what was
heard, and Insert puts it where the person writes.

**The share sheet.** The extension takes words or one web link from another app and leaves
them for the app, which puts them into the message field the next time it is in front. A link
into the Tilo site itself is opened instead.

**Words into the message field** go in as one paste (`compose` in the script). Tilo's page
takes a paste as words wherever its own text has the caret, line breaks included; typed line
by line, a line break would send a message. Where nothing has the caret the words wait for the
next field the person presses, and never go into a password field.

**When there is no page**, the app says what is wrong and offers the one thing to do: try
again, or change the site. The site is a setting (the Mac app's Settings, the phone's page in
the system's Settings), and `-TiloSite <origin>` on the command line is kept from then on.

**One app, or the desktop.** The app opens the link to one app, alone. A page that shows one
app draws no live windows inside a conversation yet; the desktop does. Until it does,
"Show the whole desktop" in the settings (`TiloDesktop`) opens the desktop with the app in
front instead.

## What the page and the host still owe

- **Answers in a notification.** The page names no `actions` today, so a notification has no
  answer buttons of its own. When it does, they appear with no change here.
- **Being told while nothing runs.** A notice is shown while the app runs, because the page
  asks for it. On a phone that is only while the app is in front. Web push does not exist in a
  system web view; telling a person whose app is closed takes a push from their host through
  Apple's service, an App ID that may receive it, and a place where the host keeps the
  device's address for the person, as it keeps a browser's for web push.
- **An answer with the app closed.** The key that proves the person is the page's. A button
  that answers while the page does not run needs a key the app itself holds.
- **A typed way to post.** Words reach the thread through the field the person writes in.
  Posting on the person's word without a caret (a reply from a notification, a share sent
  straight away) needs an action of the page the script can call.
- **Notices while the window is in view but the app is not in front.** The page tells the
  system only while it is hidden.

## Limits

- The page draws with WebGPU and reaches its host over WebTransport: system 26.4 or newer.
- The page opens no session where WebTransport offers no datagrams
  (`crates/platform/tilo-net/src/web/webtransport.rs`). The check below says whether a device
  has them.
- A site is reached over `https`, or over `http` on this computer only: a page elsewhere over
  plain `http` is no secure context, so it has neither its keys nor WebGPU.
- The live connection is HTTP/3 over UDP to the host itself. A front that carries only HTTPS
  does not carry it.

## Build, check, run

```sh
dist/apple/build.sh test                    # the package's tests; they open no window
dist/apple/build.sh mac --check --live      # build, sign, and ask the app what it can tell
dist/apple/build.sh simulator --live        # the iPhone app, asked in a simulator with no window
dist/apple/build.sh iphone --install        # build, sign, and put it on the one reachable iPhone
open "target/apple.noindex/mac/Tilo Assistant.app"  # start the Mac app
```

Builds land in `target/apple.noindex/`, where Spotlight does not look: a build is offered to a
person as an app only once someone puts it where apps go. A bundle that is open is never
replaced; quit it first.

`build.sh` reads who signs from `TILO_APPLE_SIGNING_IDENTITY`, the iPhone app's provisioning
profile from `TILO_APPLE_PROFILE`, the site from `TILO_SHELL_SITE` (default `https://tilo.sh`)
and the app from `TILO_SHELL_APP` (default `so.tilo.chat`); its head lists the rest. The
signing team is read from the identity and written into the bundles, never into the sources.
Without an identity the Mac app is signed by nobody: it runs on the Mac that built it, a later
build cannot read the keys it kept, and it has no share extension. The icon is one drawing,
`Bundle/AppIcon.svg`, drawn at every size a bundle carries when the bundle is made.

`--check` runs the app with nothing on screen and prints one line per fact: who signed it,
whether it is sandboxed, whether the web view offers a secure context, WebGPU with an adapter,
WebTransport and its datagrams, whether a key the page stored is still there on a second run,
whether a notice and words cross between page and system, and whether the site and its host
answer. `--live` also opens and closes one WebTransport session at the host the site names; it
opens no stream, so no person and no workspace is touched. A simulator has no graphics
adapter, so it fails that one line and cannot show Tilo; a real iPhone is asked from a Mac:

```sh
xcrun devicectl device process launch --console --device <id> so.tilo.assistant.shell --check --quit
```

## Before the App Store

The Mac app is sandboxed from the start with the entitlements `Bundle/Mac.entitlements`
explains; it listens on nothing, and updates nothing of itself. Still to do for a store build:
an App ID of its own (push, and an app group in place of the keychain group the phone's share
extension uses under a development profile), a universal Mac binary, an icon drawn for each
platform, the usage descriptions in every language the app ships, and review notes that say
what the page is and name each native part above.

## What is checked, and what is not

Checked with no window: the tests (settings, policy, the hand-over, the sentences against
their catalogue, the script against a page that speaks as Tilo's does), the Mac app's
`--check` in its real signature and sandbox, and the iPhone app's in a simulator.

Not checked by anything here, because it needs a window or the system's own interface: how
the page looks in the window, the menu-bar item, the settings window, dictation, the share
extension at run time, and a notification shown and answered through the system.
