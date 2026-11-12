## History: what changed in the browser's document
browser-add-bookmark = Bookmarked { $url }
browser-remove-bookmark = Removed the bookmark { $item }
browser-set-site-protection = { $enabled ->
    [true] Turned tracker protection on for { $origin }
   *[false] Turned tracker protection off for { $origin }
}
browser-remember-forms = Remembered the forms of { $origin }
browser-forget-form = Forgot a form of { $origin }
browser-forget-site-forms = Forgot the forms of { $origin }
browser-forget-all-forms = Forgot all remembered forms
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

## History: what operated a tab's page
browser-set-down = Set down what was done on a page

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
browser-service-open = Open { $name }
browser-engine-failed = Close Browser and open it again. If it still won’t start, update or reinstall Browser in Store.

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
browser-visitor-window = Not signed in
browser-visitor-open-signed-in = Open with your sign-ins

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

## A page's file chooser, shown by the system's own picker
browser-file-chooser-file = Choose a file
browser-file-chooser-files = Choose files
browser-file-chooser-folder = Choose a folder
browser-file-chooser-choose = Choose
browser-file-chooser-accepted = Accepted files

## A server's sign-in (HTTP authentication): who asks is the origin the
## credentials go to; the realm is the server's own text
browser-page-sign-in-title = Sign in to { $site }
browser-page-sign-in-title-long = Sign in to this site
browser-page-sign-in-title-proxy = Sign in to the proxy { $site }
browser-page-sign-in-title-proxy-long = Sign in to the proxy
browser-page-sign-in-asks = { $asker } asks for a user name and password.
browser-page-sign-in-asks-proxy = The proxy { $asker } asks for a user name and password.
browser-page-sign-in-realm = In its own words: “{ $realm }”.
browser-page-sign-in-clear = This connection is not private: the password can be read on its way.
browser-page-sign-in-retry = That user name or password was not accepted.
browser-page-sign-in-too-long = That user name or password is too long.
browser-page-sign-in-submit = Sign in

## The saved addresses the browser fills forms from
browser-autofill-offline = Address autofill is unavailable here: this window browses without your sign-ins.
browser-autofill-enable = Turn on address autofill
browser-autofill-disable = Turn off address autofill
browser-autofill-selected-address = the selected address
browser-autofill-delete-question = Delete { $label }?
browser-autofill-delete = Delete address
browser-autofill-keep = Keep address
browser-autofill-save = Save address
browser-autofill-discard = Discard edits
browser-autofill-add = Add address
browser-autofill-saved = Address saved on this device.

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

## The page's review: its forms, who set each answer, and what was done
browser-review-open = Review page
browser-review-title = Review
browser-review-done = Done
browser-review-empty = No forms on this page
browser-review-unreadable = Couldn’t read this page
browser-review-unreadable-hint = Reload the page and try again.
browser-review-form-untitled = Form
browser-review-form-other = Other fields
browser-review-form-in-frame = In frame { $name }
browser-review-question-unnamed = Unnamed field
browser-review-no-answer = No answer
browser-review-yes = Yes
browser-review-no = No
browser-review-secret-set = Set
browser-review-secret-not-set = Not set
browser-review-set-by = Set by { $name }
browser-review-set-by-you = Set by you
browser-review-set-typed = Typed into the page
browser-review-set-by-page = Set by the page
browser-review-set-by-no-one = No one set this
browser-review-changed = Changed since it was set
browser-review-required = Required
browser-review-invalid = Not valid
browser-review-record = Done in this tab
browser-review-earlier = { $count ->
    [one] { $count } earlier action is not shown
   *[other] { $count } earlier actions are not shown
}
browser-review-by = By { $name }
browser-review-by-you = By you
browser-review-actor-on-device = { $name } on { $device }
browser-review-failed = Failed
browser-review-did-click = Clicked { $target }
browser-review-did-fill = Filled { $target }
browser-review-did-select = Chose in { $target }
browser-review-did-files = Attached files to { $target }
browser-review-did-press = Pressed a key in { $target }
browser-review-did-key = Pressed a key
browser-review-did-hover = Pointed at { $target }
browser-review-did-drag = Dragged { $target }
browser-review-did-pointer = Used the pointer
browser-review-did-submit = Sent the form
browser-review-did-scroll = Scrolled the page
browser-review-did-reader = Switched reader view
browser-review-did-form = { $count ->
    [one] Answered { $count } question
   *[other] Answered { $count } questions
}
browser-review-did-other = Acted on the page
browser-review-did-type = Typed into { $target }
browser-review-did-type-page = Typed into the page
browser-review-by-typing = Direct input
browser-review-wrote-secret = Set, not shown
browser-review-held-submit = Submission held for you: { $what }
browser-review-held-request = Request held for you: { $what }
browser-review-held-download = Download held for you: { $what }
browser-review-held-other = Held for you: { $what }
browser-review-hold-allowed-by = Let leave by { $name }
browser-review-hold-allowed-by-you = Let leave by you
browser-review-hold-kept = Kept as it is
browser-review-hold-dropped = Dropped, nobody answered
browser-review-hold-too-many = Dropped, too many were waiting
browser-review-hold-gone = Dropped, the page went away

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
browser-site-clear-note = Clears this site’s storage, cookies and remembered forms, including cookies shared with its subdomains. Reload the page to reset data it still holds.
browser-site-forms = Remembered forms
browser-site-forms-none = No forms remembered for this site
browser-site-forms-note = The questions each form asks. Answers are not kept.
browser-site-forms-forget = Forget this site’s forms
browser-site-form-untitled = Form
browser-site-form-questions = { $count ->
    [one] { $count } question
   *[other] { $count } questions
}
browser-site-form-forget = Forget
browser-site-form-forget-name = Forget { $form }
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
browser-address-suggestions = Address suggestions
browser-window-name = Browser
browser-page-menu-name = Page context menu

## Window and accessibility copy
browser-quit-downloads-running = Downloads are still running
browser-quit-cancelling-downloads = Cancelling downloads before quitting…
browser-section-passwords = Passwords
browser-section-no-secure-page = No secure page
browser-section-address-autofill = Address autofill
browser-a11y-search-engine = Search engine
browser-a11y-clear-on-quit = Clear data when the browser closes
browser-a11y-block-ads = Block ads & trackers
browser-settings-title = Settings

## Facts, property labels and section titles
browser-save-login = Save login for { $site }?
browser-section-saved-accounts = Saved accounts
browser-section-forms = Forms on this page
browser-action-keep-page = Keep the page as it is
browser-action-close-picture-in-picture = Close picture-in-picture
browser-action-close-reader-view = Close reader view
browser-action-resume-history = Resume history in this window
browser-action-let-leave = Let it leave: { $request }

## Passwords and address autofill
browser-credentials-done = Done
browser-credentials-refresh = Refresh forms and accounts
browser-credentials-refresh-addresses = Refresh forms
browser-credentials-fill = Fill login
browser-credentials-fill-address = Fill selected address
browser-credentials-save-from-form = Save login from form
browser-credentials-delete = Delete login
browser-credentials-save-account = Save new account
browser-credentials-update-account = Update selected account
browser-credentials-delete-account = Delete account
browser-credentials-keep-account = Keep account
browser-credentials-confirm-delete = Delete this saved login?
browser-credentials-kept-here = The password is kept in this device’s password store.
browser-credentials-no-login-form = No supported login form found.
browser-credentials-no-address-form = No supported address form found on this page.
browser-credentials-no-accounts = No saved accounts for this site.
browser-credentials-unnamed-account = Unnamed account
browser-credentials-form = Form
browser-credentials-address-form = Address form
browser-credentials-create-form = Create account form
browser-credentials-sign-in-form = Sign-in form
browser-credentials-form-numbered = { $form } { $number }
browser-credentials-unavailable-private = Passwords and address autofill are unavailable here: this window browses without your sign-ins.
browser-credentials-unavailable-engine = Form filling is unavailable with this browser engine.
browser-credentials-unavailable-insecure = Open a secure HTTPS page to fill a form.
browser-credentials-checking = Checking this page…
browser-credentials-select-address = Select an enabled, saved address before filling.
browser-credentials-capture-expired = Capture expired. Read the login again.
browser-credentials-request-expired = Request expired. Refresh before trying again; an operation already started may still finish.

## What the action directory and the window name
browser-page-untitled = Web page
browser-page-content-name = Web content
browser-history-back = Back history
browser-history-forward = Forward history
browser-history-paused = History paused
browser-history-recording = History recording
browser-history-already-empty = Browsing history is already empty
browser-resource-history = History
browser-resource-bookmarks = Bookmarks
browser-resource-downloads = Downloads
browser-resource-extensions = Extensions
browser-resource-library = Library
browser-resource-settings = Settings
browser-resource-accounts = Accounts
browser-resource-account = Account
browser-resource-continuity = Bring your browsing
browser-resource-personal-bookmarks = Personal bookmarks
browser-resource-bookmark-navigation = Bookmark navigation
browser-library-already-clear = The library filter is already clear
browser-action-needs-page = Open a web page first
browser-action-private-no-forms = A private window remembers no forms
browser-action-form-changed = This form asks something else since its action was made; the page reads its forms again by itself and publishes the action of the form as it is now. If none appears, read them (read-page-forms)
browser-action-no-previous-page = No previous page
browser-action-no-next-page = No next page
browser-action-page-idle = Page is idle

## The browser's own settings pane
browser-settings-done = Done
browser-settings-manage = Manage
browser-settings-new-private-window = New private window
browser-settings-clear-history = Clear history
browser-settings-clear-site-data = Clear site data
browser-settings-general = General
browser-settings-homepage-label = Homepage
browser-settings-adblock = Block ads & trackers
browser-settings-adblock-detail = Blocks ads, trackers, and YouTube video ads.
browser-settings-clear-on-exit-label = Clear data when the browser closes
browser-settings-clear-on-exit-detail = Wipe history and cookies on exit.
browser-settings-browsing-data = Browsing data
browser-settings-browsing-data-detail = History, and the cookies and files sites keep on this device.
browser-settings-passwords = Passwords
browser-settings-passwords-detail = Sign-ins saved for sites on this device.
browser-settings-autofill = Address autofill
browser-settings-autofill-detail = Fill address forms from your saved addresses.
browser-settings-privacy = Privacy & Security
browser-settings-windows = Windows
browser-settings-private = Private browsing
browser-settings-private-detail = Nothing from a private window is kept after it closes.
browser-settings-search-engine-label = Search engine
browser-settings-search-engine-detail = Searches from the address field go here.

## What the window shows
browser-quit-keep-downloading = Keep downloading
browser-quit-cancel-downloads = Cancel downloads and quit
browser-open = Open { $address }
browser-search-for = Search for { $query }
browser-history-clear-failed = Couldn’t clear history because Browser could not save the change. Previous history was kept.
browser-download-active-hint = Finish or cancel it in Downloads in the library, then close the tab.
browser-history-paused-hint = Pages in this window are not saved to browsing history.
browser-service-sign-in-hint = Sign in to finish connecting this account.

## A window without the person's accounts
browser-no-account-custody = This window browses without your sign-ins. Open the page in a Browser window of your own to use your accounts there

## Actions

browser-address-book = Address book
browser-effect-read-continuity = Read import progress
browser-effect-open-service-session = Open account
browser-effect-retry-forget-service-session = Retry forgetting account
browser-effect-list-service-sessions = List accounts
browser-effect-read-service-session = Read account
browser-effect-retry-continuity = Retry import
browser-effect-back = Go back
browser-effect-forward = Go forward
browser-effect-navigate = Open address
browser-effect-open-tab = Open in new tab
browser-effect-open = Open page
browser-effect-show-page = Show current page
browser-effect-show-history = Show history
browser-effect-show-bookmarks = Show bookmarks
browser-effect-show-downloads = Show downloads
browser-effect-show-extensions = Show extensions
browser-effect-pin-extension = Pin to toolbar
browser-effect-unpin-extension = Unpin from toolbar
browser-effect-use-extension = Use extension
browser-effect-send-to-extension = Send text to extension
browser-effect-open-extension-settings = Extension settings
browser-effect-open-extension-side-panel = Open side panel
browser-effect-revoke-extension-access = Revoke extension access
browser-effect-remove-extension = Remove extension
browser-effect-show-settings = Browser settings
browser-effect-set-library-filter = Filter library
browser-effect-edit-address = Edit saved address
browser-effect-delete-address = Delete saved address
browser-effect-forget-visit = Forget page
browser-effect-start-download = Download address
browser-effect-show-download = Show download
browser-effect-cancel-download = Cancel download
browser-effect-retry-download = Retry download
browser-effect-open-download = Open file
browser-effect-read-download = Read file
browser-effect-save-download = Keep download
browser-effect-discard-download = Discard download
browser-effect-send-held-request = Send held request
browser-effect-read-page = Read page
browser-effect-capture-page = Capture page
browser-effect-read-page-changes = Page changes
browser-effect-read-page-element-at = Element at a point
browser-effect-wait-page-settled = Wait for page to load
browser-effect-read-page-console = Console messages
browser-effect-read-page-network = Network requests
browser-effect-find-page-element = Find page element
browser-effect-click-page = Click element
browser-effect-hover-page = Hover element
browser-effect-drag-page = Drag element
browser-effect-pointer-path = Draw pointer path
browser-effect-fill-page = Fill field
browser-effect-fill-page-secret = Fill password field
browser-effect-select-page = Choose options
browser-effect-fill-page-files = Choose files to upload
browser-effect-press-page = Press key
browser-effect-submit-page = Submit form
browser-effect-accept-page-dialog = Accept dialog
browser-effect-dismiss-page-dialog = Dismiss dialog
browser-effect-answer-permission = Answer permission request
browser-effect-pointer-page = Move pointer
browser-effect-scroll-page = Scroll page
browser-effect-import-personal-bookmarks = Import bookmarks
browser-effect-remove-personal-bookmark = Remove bookmark
browser-effect-remove-personal-imported-node = Remove imported bookmark
browser-effect-show-personal-bookmark-folder = Open bookmark folder
browser-browsing-history = Browsing history
browser-browsing-history-visit = Record a visit
browser-browsing-history-import-history-bundle = Import history
browser-browsing-history-name-visit = Name a page
browser-browsing-history-forget = Forget page
browser-browsing-history-clear-history = Forget every page
browser-personal-bookmarks = Personal bookmarks
browser-personal-bookmarks-add-bookmark = Bookmark a page
browser-personal-bookmarks-remove-bookmark = Remove a bookmark
browser-personal-bookmarks-import-bookmarks = Import bookmarks
browser-personal-bookmarks-remove-imported-node = Remove imported folder
browser-application = Browser
browser-application-add-bookmark = Bookmark a page
browser-application-remove-bookmark = Remove a bookmark
browser-application-set-site-protection = Tracker protection
