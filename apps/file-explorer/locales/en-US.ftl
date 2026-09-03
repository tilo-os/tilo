# Files — source locale (en-US).
# Collaboration, availability, history and received-share surfaces are the
# first migrated tranche. Variables are data, never preformatted Fluent source.

files-time-unknown = unknown time

## Availability actions and status
files-keep-offline = Keep Offline
files-use-automatic-storage = Use Automatic Storage
files-pinned-by-parent = Pinned by Parent Folder
files-retry-download = Retry Download
files-free-local-space = Free Local Space
files-no-pin-needed = No selected item needs an offline pin
files-no-direct-pin = No direct offline pin can be changed here
files-keeping-offline = Keeping { $count } { $count ->
    [one] item
   *[other] items
} offline
files-using-automatic = Using automatic storage for { $count } { $count ->
    [one] item
   *[other] items
}
files-offline-policy-failed = Offline policy failed: { $errors }
files-no-retryable-transfer = No selected transfer can be retried
files-retrying-transfers = Retrying { $count } { $count ->
    [one] transfer
   *[other] transfers
}…
files-retry-failed = Retry failed: { $errors }
files-no-local-space-releasable = No selected item can release local space
files-freeing-local-space = Freeing local space for { $count } { $count ->
    [one] item
   *[other] items
}…
files-free-local-space-failed = Free local space failed: { $errors }
files-eviction-needs-confirmation = Safety could not be verified; confirmation is required
files-destructive-eviction-title = Remove an unverified local copy?
files-destructive-eviction-summary = Safety checks could not prove that { $count } selected { $count ->
    [one] item has
   *[other] items have
} another fresh, durable copy.
files-destructive-eviction-no-replica = No acceptable current replica could be verified.
files-destructive-eviction-loss = If no source remains, the current file contents may become permanently unavailable.
files-destructive-eviction-protected = Retained history and blobs shared by protected files will not be removed.
files-destructive-eviction-confirm = Remove Local Copy Anyway
files-destructive-eviction-started = Destructive local removal started for { $count } { $count ->
    [one] item
   *[other] items
}.
files-permanent-delete-title = Permanently delete { $count } { $count ->
    [one] item
   *[other] items
}?
files-permanent-delete-summary = The selected { $count } { $count ->
    [one] item will
   *[other] items will
} be removed from Trash.
files-permanent-delete-loss = This cannot be undone.
files-permanent-delete-confirm = Delete Permanently
files-empty-trash-title = Permanently empty Trash?
files-empty-trash-summary = { $count ->
    [one] 1 item will be removed from Trash.
   *[other] All { $count } items will be removed from Trash.
}
files-empty-trash-confirm = Empty Trash

## Properties and availability sheet
files-property-collaborative-record = Collaborative record: { $record }
files-property-content = Content: { $hash }…
files-residency-metadata-only = metadata only
files-residency-partly-local = partly local ({ $bytes } bytes)
files-residency-local = local ({ $bytes } bytes)
files-property-on-device = On this device: { $state }
files-property-sharing = Sharing: { $count } active { $count ->
    [one] grant
   *[other] grants
}
files-property-history = History: { $versions } { $versions ->
    [one] version
   *[other] versions
}, { $conflicts } open { $conflicts ->
    [one] conflict
   *[other] conflicts
}
files-properties-collab-summary = Filesystem and verified collaborative metadata
files-availability-title = Availability — { $name }
files-availability-title-short = Availability…
files-not-stored = Not stored on this device
files-partly-stored = Partly stored ({ $bytes } bytes)
files-stored = Stored on this device ({ $bytes } bytes)
files-kept-offline-parent = Kept offline by a parent folder
files-kept-offline = Kept offline
files-automatic-storage = Automatic storage
files-replica-unverified = Replica safety has not been verified
files-replica-counts = { $reachable } reachable, { $durable } durable { $durable ->
    [one] replica
   *[other] replicas
}
files-transfer-state = Transfer: { $state }
files-transfer-idle = idle
files-transfer-queued = queued
files-transfer-downloading = downloading
files-transfer-uploading = uploading
files-transfer-verifying = verifying

## Sharing
files-search-collaborative-records = Collaborative files
files-search-record-provenance = { $path } · record { $record }
files-copy-one-time-link = Copy One-Time Share Link
files-share-composer-named = Share with { $person } · { $permission } · { $expiry }
files-share-composer-link = Copy link · { $permission } · { $expiry } · { $uses }
files-share-choice-view = View
files-share-choice-edit = Edit
files-share-choice-day = expires in 24 hours
files-share-choice-week = expires in 7 days
files-share-choice-month = expires in 30 days
files-share-choice-never = no expiry
files-share-choice-one-use = one use
files-share-choice-bounded-uses = up to { $count } uses
files-share-creating = Creating share…
files-share-invalid-options = Those sharing options cannot be combined
files-share-link-needs-expiry = Share links require an expiry
files-share-invalid-uses = Share links require a use limit from 1 to 1,000
files-share-recipient-unreachable = This person has no certified delivery device
files-sharing-access = Sharing & Access…
files-share-with = Share with { $person }
files-revoke-subject = Revoke { $subject }
files-shared-link = shared link
files-share-link-pending = A share link is already being created
files-share-change-pending = A sharing change is already in progress
files-select-collaborative-file = Select one collaborative file first
files-select-shared-file = Select the shared file first
files-sharing-unavailable = Sharing is unavailable for this workspace
files-creating-one-use-link = Creating a one-use link…
files-share-with-progress = Sharing with { $person }…
files-revoking-access = Revoking access…
files-not-shared = Not shared with anyone
files-anyone-link = Anyone with the link
files-can-view = can view
files-can-edit = can edit
files-grant-active = active
files-grant-expired = expired
files-grant-revoked = revoked
files-grant-exhausted = exhausted
files-no-expiry = no expiry
files-expires = expires { $time }
files-share-grant-line = { $subject } · { $permission } · { $status } · { $expiry }
files-sharing-title = Sharing — { $name }
files-certified-devices = { $person } · certified devices: { $devices }
files-certified-device-detail = { $device } · key { $fingerprint } · { $capabilities } · issued { $issued } · { $expiry }
files-device-cap-own = manages workspaces
files-device-cap-admit = admits devices
files-device-cap-host = trusted host
files-device-cap-read-all = reads all workspaces
files-device-cap-member = workspace member
files-sharing-summary = { $count } active { $count ->
    [one] grant
   *[other] grants
}; copied bytes cannot be recalled
files-link-copied = Share link copied to clipboard
files-share-created = Shared with { $person }; downloaded copies cannot be recalled
files-share-created-pending = Shared with { $person }; delivery is pending
files-share-created-delivered = Shared with { $person }; delivery completed
files-access-revoked = Access revoked; previously downloaded bytes cannot be recalled
files-share-inactive = That share was already inactive

## History and conflict resolution
files-version-history = Version History…
files-restore-previous = Restore Previous Version
files-resolve-keep-current = Resolve { $conflict } — Keep Current
files-resolve-use-other = Resolve { $conflict } — Use Other (device { $device })
files-history-version-line = { $action } · { $time } · by { $person } · device { $device }
files-history-older = Older versions are available from the history service
files-conflict-needs-review = Needs review
files-conflict-resolved-strategy = Resolved: { $strategy }
files-conflict-resolved = Resolved
files-strategy-keep-current = Keep Current
files-strategy-use-other = Use Other
files-strategy-keep-both = Keep Both
files-strategy-merge = Merge
files-conflict-line = Conflict { $conflict } · { $count } { $count ->
    [one] version
   *[other] versions
} · { $state }
files-conflict-resolve-hint = Resolve from the context menu: Keep Current or Use Other
files-conflict-unsupported-hint = Keep Both and Merge are unavailable until their signed workflows are supported
files-history-title = Version History — { $name }
files-history-summary = { $versions } { $versions ->
    [one] version
   *[other] versions
}, { $heads } current { $heads ->
    [one] head
   *[other] heads
}
files-history-change-pending = A history change is already in progress
files-select-file-with-history = Select one collaborative file with history first
files-version-restored = Version restored as a new history entry
files-restoring-version = Restoring that version…
files-keeping-current = Keeping the current version…
files-applying-other = Applying the other version…
files-merging-versions = Safely merging non-overlapping changes…
files-conflict-workflow-required = Keep Both and Merge require a dedicated safe workflow
files-conflict-resolved-signed = Conflict resolved with a signed history entry
files-history-change-saved = History change saved
files-history-action-created = Created
files-history-action-edited = Edited
files-history-action-renamed = Renamed
files-history-action-moved = Moved
files-history-action-deleted = Deleted
files-history-action-conflict-resolved = Resolved conflict
files-history-action-restored = Restored
files-history-action-changed = Changed
files-history-current-version = { $action } · Current version
files-history-timeline-detail = { $person } · { $time }
files-history-preview-title = Selected version
files-history-compare-title = Compare with current version
files-history-selected-action = Action: { $action }
files-history-selected-by = By: { $person }
files-history-selected-at = When: { $time }
files-history-version-id = Version: { $id }
files-history-content-identity = Content identity: { $hash }
files-history-metadata-identity = Metadata identity: { $hash }
files-history-no-content = no file content
files-history-compare-content = Content: { $state }
files-history-compare-metadata = Metadata: { $state }
files-history-same = unchanged
files-history-different = changed
files-history-show-activity = Show in Workspace Activity
files-history-compare = Compare with Current
files-history-hide-compare = Show Version Details
files-history-restore-selected = Restore This Version
files-history-current-version-label = Current Version
files-history-timeline-a11y = Version timeline
files-history-timeline-a11y-item = { $action } by { $person }, { $time }

## Accessible collaborative state
files-a11y-transfer-failed = Transfer failed
files-a11y-transfer-queued = Transfer queued
files-a11y-downloading = Downloading
files-a11y-uploading = Uploading
files-a11y-verifying = Verifying
files-a11y-online-only = Online only
files-a11y-partly-offline = Partly available offline
files-a11y-available-offline = Available offline
files-a11y-kept-offline-parent = Kept offline by parent folder
files-a11y-kept-offline = Kept offline
files-a11y-replica-unknown = Replica status unknown
files-a11y-durably-replicated = Durably replicated
files-a11y-not-durably-replicated = Not durably replicated
files-a11y-shared-recipients = Shared with { $count } { $count ->
    [one] recipient
   *[other] recipients
}
files-a11y-version-conflict = Has a version conflict

## Service and received-share errors/status
files-history-encode-failed = Could not encode the history request
files-history-request-too-large = History request exceeds its wire bound
files-history-restore-failed = Could not restore that version: { $error }
files-conflict-strategy-unsupported = That conflict strategy is not supported by Files yet
files-conflict-encode-failed = Could not encode the conflict request
files-conflict-request-too-large = Conflict request exceeds its wire bound
files-conflict-resolve-failed = Could not resolve that conflict: { $error }
files-share-wrong-response = Share service returned the wrong response
files-share-runtime-failed = Could not start the share client: { $error }
files-share-unavailable-error = Share service unavailable: { $error }
files-share-encode-failed = Could not encode share request: { $error }
files-share-request-too-large = Share request exceeds its wire bound
files-share-request-failed = Could not request link: { $error }
files-share-flush-failed = Could not finish the share request: { $error }
files-share-no-response = Share service did not respond: { $error }
files-share-response-too-large = Share service returned an oversized response
files-share-response-truncated = Share response was truncated: { $error }
files-share-response-invalid = Invalid share-service response: { $error }
files-share-refused = Share service refused the request ({ $code })
files-received-refused = Received-file service refused the link ({ $code })
files-received-retry-refused = Received-file service could not retry ({ $code })
files-received-wrong-response = Received-file service returned the wrong response
files-received-runtime-failed = Could not start the received-file client: { $error }
files-received-encode-failed = Could not encode received-file request
files-received-request-too-large = Received-file request exceeds its bound
files-received-unavailable = Received-file service is unavailable
files-received-no-response = Received-file service did not respond
files-received-response-too-large = Received-file service returned an oversized response
files-received-response-truncated = Received-file service response was truncated
files-received-response-invalid = Received-file service returned an invalid response
files-received-not-available = Received-file service is not available
files-received-fd-clone-failed = Received-file service descriptor could not be cloned
files-received-fd-invalid = Received-file service descriptor is invalid
files-received-file = Received shared file
files-received-mount-starting = Shared file admitted; mount is starting
files-received-admitted = Shared file admitted
files-received-link-read-failed = Could not read the shared-file link
files-received-link-too-large = Shared-file link exceeds its bound
files-received-link-invalid-text = Shared-file link is not valid text
files-received-link-empty = Shared-file link is empty

## Application chrome and navigation
files-app-title = Files
files-back = Back
files-forward = Forward
files-up = Up one folder
files-view-list = List
files-view-grid = Grid
files-sort-by = Sort by
files-view-options = View options
files-path-placeholder = Type a path, e.g. ~/Documents
files-place-home = Home
files-place-favourites = Favourites
files-place-favorites = Favorites
files-place-system = System
files-place-root = Root
files-place-trash = Trash
files-place-workspace = Workspace
files-place-documents = Documents
files-place-downloads = Downloads
files-place-pictures = Pictures
files-place-music = Music
files-place-videos = Videos

## List, empty/error state and accessibility
files-column-name = Name
files-column-size = Size
files-column-modified = Modified
files-try-again = Try Again
files-loading = Loading…
files-loading-folder = Loading { $path }
files-directory-read-failed = Could not open this folder: { $error }
files-a11y-load-error = Could not load folder: { $error }. Try Again is available
files-a11y-panel-details = { $title }. { $summary }. { $details }. Press Escape to close
files-a11y-dialog-close = { $summary }. Press Escape to close
files-a11y-activity = Files activity
files-a11y-current-folder = Current folder: { $path }
files-a11y-places = Places
files-a11y-favourite = Favourite
files-a11y-system-location = System location
files-a11y-place-description = { $section }, { $path }
files-a11y-breadcrumbs = Breadcrumbs
files-a11y-current-breadcrumb = Current folder, { $path }. Press Enter to edit the path
files-a11y-open-folder = Open { $path }
files-empty-trash = Trash is empty
files-empty-folder = This folder is empty
files-item-count = { $count } { $count ->
    [one] item
   *[other] items
}
files-selected-count = { $base }  ·  { $count } selected
files-a11y-list = Files
files-a11y-folder = Folder
files-a11y-file = File, { $size }
files-new-name-placeholder = New name

## Properties
files-property-type = Type: { $type }
files-property-location = Location: { $location }
files-property-size = Size: { $size }
files-property-modified = Modified: { $time }
files-property-dimensions = Dimensions: { $width } × { $height } px
files-property-duration = Duration: { $duration }
files-property-pages = Pages: { $count }
files-property-codec = Codec: { $codec }
files-property-container = Container: { $container }
files-property-unknown = Unknown
files-properties-title = Properties — { $name }
files-properties-summary = Filesystem metadata

## Ordinary operation results
files-new-folder-default-name = New Folder
files-created-folder = Created folder
files-new-folder-failed = New folder failed: { $error }
files-permanently-deleted = Permanently deleted { $count } { $count ->
    [one] item
   *[other] items
}
files-delete-failed = Delete failed: { $error }
files-moved-to-trash = Moved { $count } { $count ->
    [one] item
   *[other] items
} to Trash
files-restored = Restored { $count } { $count ->
    [one] item
   *[other] items
}
files-restore-failed = Restore failed: { $error }
files-trash-already-empty = Trash is already empty
files-emptied-trash = Emptied Trash ({ $count })
files-empty-trash-failed = Empty Trash failed: { $error }
files-copied = Copied { $count } { $count ->
    [one] item
   *[other] items
}
files-cut = Cut { $count } { $count ->
    [one] item
   *[other] items
}
files-created-file = Created file
files-new-file-failed = New file failed: { $error }
files-added-favourite = Added to Favourites
files-removed-favourite = Removed from Favourites
files-moved-with-failures = Moved { $moved } to “{ $destination }” ({ $failed } failed)
files-moved-to-folder = Moved { $count } { $count ->
    [one] item
   *[other] items
} to “{ $destination }”
files-nothing-to-move = Nothing to move
files-pasted = Pasted { $count } { $count ->
    [one] item
   *[other] items
}
files-pasted-with-failures = Pasted { $count } ({ $failed } failed)
files-renamed = Renamed
files-rename-failed = Rename failed: { $error }

## Context, view and sort menus
files-menu-open = Open
files-menu-restore = Restore
files-menu-delete-permanently = Delete Permanently
files-menu-empty-trash = Empty Trash
files-menu-rename = Rename
files-menu-cut = Cut
files-menu-copy = Copy
files-menu-properties = Properties…
files-menu-paste = Paste
files-menu-new-folder = New Folder
files-menu-new-file = New File  ›
files-menu-move-trash = Move to Trash
files-menu-add-favourite = Add to Favourites
files-menu-remove-favourite = Remove from Favourites
files-menu-hide-hidden = Hide Hidden Files
files-menu-show-hidden = Show Hidden Files
files-menu-show-hidden-checked = ✓ Show Hidden Files
files-menu-sort-name = Sort by Name
files-menu-sort-size = Sort by Size
files-menu-sort-type = Sort by Type
files-menu-sort-date = Sort by Date
files-menu-open-terminal = Open Terminal Here
files-sort-name = Name
files-sort-size = Size
files-sort-type = Type
files-sort-date = Date
files-sort-ascending = Ascending
files-sort-descending = Descending
files-menu-checked = ✓ { $label }
files-menu-unchecked =      { $label }
files-new-from-template = New { $name }
files-drag-items = { $count } items

## File-operation validation
files-folder-name-empty = Folder name cannot be empty
files-folder-name-slash = Folder name cannot contain '/'
files-name-empty = Name cannot be empty
files-name-slash = Name cannot contain '/'
files-no-parent = No parent directory
files-name-exists = “{ $name }” already exists
files-source-no-name = Source has no file name
