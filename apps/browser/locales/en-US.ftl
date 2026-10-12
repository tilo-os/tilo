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
