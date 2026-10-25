feeds-open-link-invalid = This article link cannot be opened. Only web, Gemini and email links are supported.
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
