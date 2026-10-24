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
