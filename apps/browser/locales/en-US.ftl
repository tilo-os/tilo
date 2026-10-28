## History: what changed in the browser's document
browser-add-bookmark = Bookmarked { $url }
browser-remove-bookmark = Removed the bookmark { $item }
browser-set-site-protection = { $enabled ->
    [true] Turned tracker protection on for { $origin }
   *[false] Turned tracker protection off for { $origin }
}
browser-browser-bookmarks = Changed the bookmarks
browser-bookmark-title = Renamed the bookmark { $item }
browser-settings-adblock-enabled = { $new ->
    [true] Turned ad and tracker blocking on
   *[false] Turned ad and tracker blocking off
}
browser-settings-clear-on-exit = { $new ->
    [true] Turned on clearing browsing data on exit
   *[false] Turned off clearing browsing data on exit
}
browser-settings-search-engine = Changed the search engine
browser-settings-homepage = Changed the homepage

## History: the person's browsing history
browser-visit = Visited { $url }
browser-forget = Forgot { $url }
browser-clear-history = Cleared the browsing history

## History: the person's address book
browser-address-book-enabled = { $new ->
    [true] Turned address autofill on
   *[false] Turned address autofill off
}
browser-address-book-profiles = Changed the saved addresses
browser-address-profile-id = Changed the id of the address { $item }
browser-address-profile-label = Renamed the address { $item }
browser-address-profile-full-name = Changed the full name of the address { $item }
browser-address-profile-given-name = Changed the given name of the address { $item }
browser-address-profile-family-name = Changed the family name of the address { $item }
browser-address-profile-street = Changed the street of the address { $item }
browser-address-profile-city = Changed the city of the address { $item }
browser-address-profile-region = Changed the region of the address { $item }
browser-address-profile-postal-code = Changed the postal code of the address { $item }
browser-address-profile-country = Changed the country of the address { $item }

## Personal migration library and recovery
browser-personal-bookmarks-back = Back
browser-personal-bookmarks-previous = Previous
browser-personal-bookmarks-next = Next
browser-personal-bookmarks-empty = No bookmarks here
browser-personal-bookmarks-remove = Remove bookmark item
browser-personal-bookmarks-unnamed-folder = Untitled folder
browser-workspace-bookmarks = Workspace bookmarks
browser-continuity-failed = We couldn’t finish bringing your browsing over.
browser-continuity-retry = Try again
browser-service-forget = Forget account on this device
browser-service-forgetting = Forgetting this account on this device…
browser-service-forgotten = Local website session removed
browser-service-custody-backend-needed = Saved account credentials have not been revoked. A custody backend is needed to remove them.
browser-service-forget-open-first = Open the selected account in this window first.
browser-service-forget-wait = Wait for the selected account to finish opening.
browser-service-forget-fence-failed = The selected account could not be fenced for forgetting.

browser-service-forget-retry = Retry forgetting this account
browser-service-forget-retry-failed = Account deletion could not be completed. Saved credentials have not been revoked; retry forgetting this account.
browser-engine-repair-message = The built-in web component is missing. Review its repair in Store; nothing downloads without your consent.
browser-engine-repair-action = Review repair
browser-engine-reopen = Close Browser and open it again.

## Toolbar
browser-back = Back; hold or right-click for history
browser-back-tooltip = Back (Alt+Left); hold or right-click for history
browser-forward = Forward; hold or right-click for history
browser-forward-tooltip = Forward (Alt+Right); hold or right-click for history
browser-stop = Stop loading
browser-reload = Reload
browser-reload-tooltip = Reload (Ctrl+R)
browser-address = Address and search
browser-address-placeholder = Search or enter address
browser-go = Go
browser-suggestion-search = Search with { $engine }
browser-suggestion-bookmark = Bookmark, { $url }
browser-page-bookmarked = Page bookmarked
browser-bookmark-page = Bookmark this page
browser-library-open = Open browser library
browser-library-close = Close browser library
browser-more-commands = More commands
browser-private = Private
browser-private-window = Private window
browser-private-tooltip = Browsing data is removed when this window closes. Files you download remain.

## The More menu
browser-menu-site-information = Site information
browser-menu-library = History, bookmarks and downloads
browser-menu-picture-in-picture = Picture in Picture
browser-menu-reader-view = Reader view
browser-menu-pause-history = Pause history
browser-menu-find = Find in page
browser-menu-developer-tools = Developer tools
browser-menu-zoom-in = Zoom in
browser-menu-zoom-out = Zoom out
browser-menu-actual-size = Actual size
browser-menu-actual-size-zoomed = Actual size ({ $percent }%)
browser-menu-ads-and-trackers = Ads and trackers
browser-menu-new-private-window = New private window
browser-menu-settings = Settings
browser-menu-close-settings = Close settings
browser-menu-page = Page
browser-menu-zoom = Zoom
browser-menu-privacy = Privacy
browser-menu-extensions = Extensions
browser-menu-manage-extensions = Manage extensions
browser-extension-add = Add to Tilo Browser
browser-extension-adding = Adding extension
browser-extension-side-panel-close = Close side panel
browser-extension-side-panel = { $name } side panel
browser-extension-side-panel-width = Side panel width
browser-extension-can = It can
browser-extension-access-all-sites = Read and change all your data on all websites
browser-extension-access-site = Read and change your data on { $site }
browser-extension-access-sites = Read and change your data on { $count } websites
browser-extension-block-content = Block content on any page
browser-extension-read-history = Read your browsing history
browser-extension-change-history = Read and change your browsing history
browser-extension-bookmarks = Read and change your bookmarks
browser-extension-downloads = Manage your downloads
browser-extension-clipboard-read = Read data you copy and paste
browser-extension-clipboard-write = Change data you copy and paste
browser-extension-notifications = Show notifications
browser-extension-location = Detect your physical location
browser-extension-management = Manage your apps, extensions and themes
browser-extension-native = Communicate with programs on this device
browser-extension-privacy = Change your privacy settings
browser-extension-top-sites = Read a list of your most visited websites
browser-extension-screen = Capture the content of your screen
browser-extension-debugger = Inspect and control the pages you open
browser-extension-no-access = It needs no special access
browser-extension-no-version = This extension no longer runs in Chrome. Many have a newer edition, such as uBlock Origin Lite for uBlock Origin.
browser-extension-update-asks = { $name } asks for more access since its update. Turn it on to allow it.
browser-extension-notification-progress = { $percent }% done
browser-extension-notification-from = From { $name }
browser-extension-asks = { $name } asks for more access. If you allow it, it can
browser-extension-added = { $name } was added
browser-extension-offer = Add { $name } to Tilo Browser?
browser-extension-version = Version { $version }
browser-extension-confirm = Add extension
browser-extension-cancel = Cancel
browser-extension-close = Close
browser-extension-turn-on = Turn on
browser-extension-turn-off = Turn off
browser-extension-settings = Settings
browser-extension-pin = Pin
browser-extension-unpin = Unpin
browser-extension-remove = Remove
browser-extension-revoke = Revoke access
browser-extension-granted = Allowed later: { $access }
browser-extension-granted-quiet = You allowed it more access later
browser-extension-nothing-granted = This extension has no access you allowed later
browser-extension-pin-to-toolbar = Pin { $name } to the toolbar
browser-extension-running = Running
browser-extension-starting = Starting…
browser-extension-off = Off
browser-extension-unsupported = Not supported in Chrome anymore
browser-extension-access-unknown = Its access could not be read
browser-extension-how-to-add = Open an extension on the Chrome Web Store and choose Add to Tilo Browser, or install a downloaded .crx or .zip file.
browser-extension-file = Extension package file
browser-extension-file-placeholder = Path to a .crx or .zip file
browser-extension-install-file = Install from file
browser-extension-store-failed = The extension could not be downloaded from the Chrome Web Store. Check your connection and try again.
browser-extension-too-large = This extension is too large to add.
browser-extension-damaged = This extension package is damaged or incomplete. Download a fresh copy and try again.
browser-extension-broken = This extension is broken and cannot be added. Look for a newer version from its maker.
browser-extension-not-saved = The extension could not be saved on this device. Free up some space and try again.
browser-extension-file-missing = No file was found at this path. Check the path and try again.
browser-extension-file-unreadable = This file could not be opened. Check that you may read it and try again.
browser-extension-turn-on-failed = { $name } could not be turned on. Try again.
browser-extension-turn-off-failed = { $name } could not be turned off. Try again.
browser-extension-remove-failed = { $name } could not be removed. Try again.
browser-extension-pin-failed = { $pinned ->
    [true] { $name } could not be pinned to the toolbar. Try again.
   *[false] { $name } could not be unpinned from the toolbar. Try again.
}
browser-extension-load-failed = { $name } could not be turned on. Remove it and add it again.
browser-extension-unreadable = Extensions could not be loaded. Close Browser and open it again.
browser-extension-page-failed = The extension's page could not open. Try again.
browser-extension-window-none = This window has no extensions
browser-extension-not-added = This extension is not added
browser-extension-runs-no-more = This extension no longer runs in Chrome
browser-extension-no-button = This extension is off or has no toolbar button
browser-extension-no-settings = This extension is off or has no settings page
browser-extension-open-store-first = Open an extension's page on the Chrome Web Store first
browser-extension-already-added = This extension is already added
browser-extension-another-adding = Another extension is being added
browser-page-stopped = This page stopped working
browser-popup-close = Close this window

## A page's own questions (alert, confirm, prompt, leave this page)
browser-page-dialog-message-title = Message from { $site }
browser-page-dialog-message-title-page = Message from this page
browser-page-dialog-question-title = { $site } asks
browser-page-dialog-question-title-page = This page asks
browser-page-dialog-close = Close
browser-page-dialog-allow = Allow
browser-page-dialog-send = Send
browser-page-dialog-cancel = Cancel
browser-page-dialog-leave-title = Leave this page?
browser-page-dialog-reload-title = Reload this page?
browser-page-dialog-unsaved = Changes you made may not be saved.
browser-page-dialog-leave = Leave
browser-page-dialog-reload = Reload

## An address or page that did not open
browser-address-failed = Couldn’t open this address
browser-address-empty = Enter a web address or search words
browser-address-invalid = This address can’t be opened. Check it and try again.
browser-address-not-web = Only web addresses open here. Enter one that starts with https://
browser-page-failed = Couldn’t open this page
browser-page-failed-not-found = No site has this address. Check it for typos and try again.
browser-page-failed-offline = You’re offline. Connect to the internet and try again.
browser-page-failed-no-answer = The site didn’t answer. Try again in a moment.
browser-page-failed-not-secure = The site’s secure connection failed, so the page wasn’t opened. Try again later.
browser-page-failed-blocked = This page is blocked and can’t be opened here.
browser-page-failed-retry = The page didn’t load. Try again.
browser-page-retry = Try again

## The Library: its sections, its empty states and its rows
browser-library-title = Library
browser-library-done = Done
browser-library-section-history = History
browser-library-section-bookmarks = Bookmarks
browser-library-section-downloads = Downloads
browser-library-section-extensions = Extensions
browser-library-pause = Pause history
browser-library-resume = Resume history
browser-library-clear-filter = Clear library filter
browser-library-no-match = No matching items
browser-library-history-paused = History is paused in this window
browser-library-history-empty = No browsing history yet
browser-library-bookmarks-empty = No bookmarks yet
browser-library-bookmarks-empty-hint = Select the star in the address bar to keep a page.
browser-library-extensions-empty = No extensions installed
browser-library-remove-bookmark = Remove bookmark { $title }
browser-library-forget-visit = Remove { $title } from history

## Downloads: the list in the Library and its progress
browser-downloads-empty = No downloads yet
browser-download-open = Open
browser-download-cancel = Cancel
browser-download-cancel-name = Cancel download { $name }
browser-download-reveal = Show in Files
browser-download-reveal-name = Show { $name } in Files
browser-download-retry = Retry
browser-download-retry-name = Retry download { $name }
browser-download-cannot-retry = Can’t retry
browser-download-downloading = Downloading { $name }
browser-download-waiting-destination = Waiting for a save destination…
browser-download-progress = { $received } of { $total }
browser-download-received = { $received } downloaded
browser-download-complete = Download complete
browser-download-saved-to = Saved to { $path }
browser-download-failed = The download failed
browser-download-failed-notice = Download failed
browser-download-cancelled = Cancelled
browser-downloads-progress-status = { $count ->
    [one] { $count } download at { $percent }%
   *[other] { $count } downloads at { $percent }%
}
browser-downloads-active-status = { $count ->
    [one] { $count } active download
   *[other] { $count } active downloads
}
browser-download-prompt = Download { $name } from { $url }? Save in a new folder in Downloads.
browser-download-save = Save
browser-download-unavailable = The downloaded file is no longer available.
browser-download-open-failed = Could not open the download. Try revealing its folder.

## A site asking for a permission
browser-permission-request = { $origin } requests { $rights }. Expires in { $seconds } seconds.
browser-permission-allow = Allow
browser-permission-deny = Deny

## The site panel: information, permissions and tracker protection
browser-site-information = Site information
browser-site-launcher-tooltip = Connection, permissions and stored data
browser-site-page-protection = Protection
browser-site-page-connection = Connection and data
browser-site-protection = Ads and trackers
browser-site-done = Done
browser-site-no-origin = No HTTP or HTTPS origin
browser-site-connection-secure = Connection is secure
browser-site-connection-mixed = Encrypted connection with mixed content
browser-site-connection-insecure = HTTP connection is not encrypted
browser-site-connection-unverified = Connection security is not verified
browser-site-certificate = Certificate
browser-site-issuer = Issuer
browser-site-connection = Connection
browser-site-certificate-problem = Certificate problem
browser-site-status-code = Status code { $code }
browser-site-expires = Expires
browser-site-permission-camera = Camera
browser-site-permission-microphone = Microphone
browser-site-permission-location = Location
browser-site-permission-notifications = Notifications
browser-site-permission-ask = Ask
browser-site-permission-blocked = Blocked
browser-site-permission-allowed = Allowed
browser-site-block-permissions = Block permissions
browser-site-block-note = Blocks camera, microphone, location and notifications. Reload the page to end sessions already running.
browser-site-ask-again = Ask again
browser-site-clear-data = Clear site data
browser-site-clear-note = Clears this site’s storage and cookies, including cookies shared with its subdomains. Reload the page to reset data it still holds.
browser-site-protection-off-settings = Protection is off in Settings
browser-site-protection-off = Protection is off for this site
browser-site-protection-on = Protection is on for this site
browser-site-blocked-count = { $count } requests blocked or replaced in this page
browser-site-protection-enable = Turn on and reload
browser-site-protection-disable = Turn off and reload
browser-site-filters-updated-on = Filters last updated { $date }
browser-site-filters-update = Update filters
browser-site-working = Working…
browser-site-query-timeout = Site information timed out; retrying…
browser-site-action-timeout = The action timed out. Reload the page before trying it again.
browser-site-reading = Reading site information…
browser-site-tls-unknown = TLS version unavailable
browser-site-unavailable = unavailable

## The filter list the site panel reports
browser-filters-built-in = Using the built-in filter list
browser-filters-updating = Updating filter lists…
browser-filters-updated = Filter lists updated
browser-filters-cached = Using cached filter lists
browser-filters-failed = Update failed: { $detail }. Previous rules retained.
browser-filters-failed-worker = Update failed: worker unavailable. Previous rules retained.
