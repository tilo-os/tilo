feeds-open-link-invalid = Tilo can’t open this link. It opens web, Gemini and email links.
feeds-open-link-started = Opening link in the default app…
feeds-open-link-failed = Could not open the link

## History: what each change to the library did, by field and by verb

feeds-article-read = { $new ->
    [true] Marked { $item } as read
   *[false] Marked { $item } as unread
}
feeds-article-starred = { $new ->
    [true] Starred { $item }
   *[false] Unstarred { $item }
}
feeds-article-tags = Changed the tags of { $item }
feeds-feed-title = Renamed the subscription { $item }
feeds-feed-folder = Moved { $item } to another folder
feeds-folder-name = Renamed the folder { $item }
feeds-folder-collapsed = { $new ->
    [true] Folded the folder { $item }
   *[false] Unfolded the folder { $item }
}
feeds-library-folders = Changed the folders
feeds-library-feeds = Changed the subscriptions
feeds-library-articles = Changed the articles
feeds-add-subscription = Subscribed to { $url }
feeds-unsubscribe = Unsubscribed from { $item }
feeds-mark-feed-read = Marked everything in { $item } as read
feeds-mark-everything-read = Marked all articles as read
feeds-new-folder = Created the folder { $name }
feeds-delete-folder = Deleted the folder { $item }
feeds-import-subscriptions = Subscribed to what an OPML file lists
feeds-refreshed = Brought in new articles

## A subscription that failed to refresh, as its row says it

feeds-refresh-unreachable = Couldn't reach the site
feeds-refresh-refused = The site refused the request
feeds-refresh-refused-status = The site refused the request (HTTP { $status })
feeds-refresh-not-a-feed = The answer wasn't a feed
feeds-refresh-untrusted = The site's certificate changed, so someone may be impersonating it
feeds-refresh-unavailable = Couldn't start fetching
feeds-window-name = Feeds
feeds-reading-settings = Reading settings
feeds-refreshing = Refreshing feeds
feeds-no-article = No article selected
feeds-articles-with-tag = Show articles with this tag
feeds-feed-actions = Feed actions

## Window and accessibility copy
feeds-a11y-text-size = Text size
feeds-a11y-subscriptions = Subscriptions

## Facts, property labels and section titles
feeds-a11y-star = Star { $article }
feeds-a11y-tag = Tag { $tag }

## What the window shows
feeds-add-caption = Paste the address of an RSS, Atom or Gemini feed, or of a website that offers one.
feeds-add-label = Feed URL
feeds-folder-caption = Folders keep related subscriptions together.
feeds-folder-label = Folder name
feeds-folder-placeholder = Newsletters
feeds-rename-caption = Choose any name for this feed.
feeds-rename-label = Feed name
feeds-tags-caption = Separate tags with commas.
feeds-tags-label = Tags

## What the overview, a refresh and the reading settings say
feeds-status-refreshing = Refreshing { $done } of { $total } feeds
feeds-status-new = { $count ->
    [one] 1 new article
   *[other] { $count } new articles
}
feeds-status-failed = { $count ->
    [one] 1 feed could not be refreshed
   *[other] { $count } feeds could not be refreshed
}
feeds-status-up-to-date = All feeds are up to date
feeds-filter-all = All articles
feeds-filter-unread = Unread
feeds-filter-starred = Starred
feeds-filter-folder = Folder
feeds-filter-feed = Feed
feeds-filter-tag = Tag
feeds-theme-system = System
feeds-theme-light = Light
feeds-theme-sepia = Sepia
feeds-theme-dark = Dark
feeds-font-sans = Sans
feeds-font-serif = Serif
feeds-font-mono = Mono

## Actions

feeds-effects-refresh-all = Refresh all subscriptions
feeds-effects-import-opml = Import OPML…
feeds-effects-new-folder-form = New folder…
feeds-effects-export-opml = Export OPML…
feeds-effects-reveal-tag = Show tagged articles
feeds-effects-reader-text-larger = Increase reader text size
feeds-effects-reader-text-smaller = Decrease reader text size
feeds-effects-reader-font = Use reader font
feeds-effects-reader-paper = Use reader paper
feeds-library = Feeds
feeds-library-add-subscription = Add subscription
feeds-library-unsubscribe = Unsubscribe
feeds-library-mark-feed-read = Mark a subscription read
feeds-library-mark-everything-read = Mark all articles as read
feeds-library-new-folder = New folder
feeds-library-delete-folder = Delete folder
feeds-library-import-subscriptions = Import subscriptions
feeds-library-refreshed = Record fetched articles

## What the app is called in a window and in the launcher
feeds-app-title = Feeds
