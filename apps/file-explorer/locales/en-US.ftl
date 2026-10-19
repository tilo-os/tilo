# Files — source locale (en-US).
# Collaboration, availability, history and received-share surfaces are the
# first migrated tranche. Variables are data, never preformatted Fluent source.

files-time-unknown = unknown time

## Keyboard shortcuts
files-shortcuts-title = Keyboard shortcuts
files-shortcuts-row = { $action }: { $keys }
files-shortcuts-preview = Preview
files-shortcuts-open = Open
files-shortcuts-back-forward = Back, forward
files-shortcuts-up = Enclosing folder
files-shortcuts-location = Go to a location
files-shortcuts-rename = Rename
files-shortcuts-new-folder = New folder
files-shortcuts-trash = Move to Trash
files-shortcuts-copy-cut-paste = Copy, cut, paste
files-shortcuts-undo-redo = Undo, redo
files-shortcuts-select-all = Select all
files-shortcuts-hidden = Show hidden files
files-shortcuts-size = Larger, smaller items
files-shortcuts-refresh = Refresh
files-selection-empty = Select files first.
files-clipboard-source-unavailable = A selected file has changed or is no longer available. Select it again before copying.
files-clipboard-source-changed = The shown object differs from its retained selection. Select it again before copying.
files-clipboard-unavailable = Copy and paste isn’t available right now.
files-clipboard-empty = The workspace file clipboard is empty.
files-clipboard-undone = Paste undone.
files-selection-unavailable = That item is no longer in this folder.
files-selection-too-large = Select at most 256 items in one request.
files-copy-too-large = Copy items from one folder at a time when there are more than 256.

## Availability actions and status
files-opening-document = Opening document…
files-document-opening-cancelled = Document opening was cancelled
files-keep-offline = Keep offline
files-use-automatic-storage = Use automatic storage
files-pinned-by-parent = Pinned by parent folder
files-retry-download = Retry download
files-free-local-space = Free local space
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
files-destructive-eviction-confirm = Remove local copy anyway
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
files-permanent-delete-confirm = Delete permanently
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
files-properties-collab-summary = File and sharing details
files-availability-title = Availability of { $name }
files-availability-title-short = Availability…
files-not-stored = Not stored on this device
files-partly-stored = Partly stored ({ $bytes } bytes)
files-stored = Stored on this device ({ $bytes } bytes)
files-kept-offline-parent = Kept offline by a parent folder
files-kept-offline = Kept offline
files-automatic-storage = Automatic storage
files-replica-unverified = Not known yet whether other devices keep a copy
files-replica-counts = Kept on { $durable } other { $durable ->
    [one] device
   *[other] devices
}
files-transfer-state = Transfer: { $state }
files-transfer-idle = idle
files-transfer-queued = queued
files-transfer-downloading = downloading
files-transfer-uploading = uploading
files-transfer-verifying = verifying

## Sharing
files-search-collaborative-records = Collaborative files
files-search-record-provenance = { $path }
files-copy-one-time-link = Copy one-time share link
files-import-once = Import once to local folder…
files-cloud-title = Cloud files
files-account-import-only = Import only
files-cloud-enumerating = Reading folders
files-cloud-exporting = Exporting document
files-cloud-copying = Copying
files-cloud-done = Completed
files-cloud-cancelled = Cancelled
files-cloud-failed = Failed
files-cloud-preparing = Preparing
files-cloud-independent = Select files or folders, then choose a local destination. Copies are editable and independent; cloud originals stay unchanged.
files-cloud-root = All files
files-cloud-converted = Google document. Imports as .docx; conversion can change formatting.
files-cloud-converted-sheet = Google spreadsheet. Imports as .xlsx; conversion can change formatting.
files-cloud-converted-slides = Google presentation. Imports as .pptx; conversion can change formatting.
files-cloud-loading = Reading cloud folder…
files-cloud-busy = A cloud request is already running.
files-cloud-invalid-reply = The cloud service returned an unexpected reply.
files-cloud-selected = Selected: { $name }
files-cloud-select = Select
files-cloud-more = Load more
files-cloud-import = Import once…
files-cloud-destination = Choose a local destination folder
files-cloud-progress = { $phase }: { $count } files, { $bytes } bytes
files-cloud-cancel = Cancel import
files-cloud-retry = Retry failed
files-cloud-reveal = Show destination
files-import-source-unavailable = Choose an entry in a mounted Dropbox or Google Drive account.
files-import-local-destination = One-time import needs a local destination, not a synced cloud folder.
files-share-composer-named = Share with { $person } ({ $permission }, { $expiry })
files-share-composer-link = Copy link ({ $permission }, { $expiry }, { $uses })
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
files-share-recipient-unreachable = None of this person’s devices can receive files right now
files-sharing-access = Sharing and access…
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
files-share-grant-line = { $subject }: { $permission }, { $status }, { $expiry }
files-sharing-title = Share { $name }
files-certified-devices = { $person }, devices: { $devices }
files-certified-device-detail = { $device }, key { $fingerprint }, { $capabilities }, issued { $issued }, { $expiry }
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
files-version-history = Version history…
files-restore-previous = Restore previous version
files-resolve-keep-current = Resolve { $conflict }: keep current
files-resolve-use-other = Resolve { $conflict }: use other (device { $device })
files-history-version-line = { $action } { $time } by { $person } on device { $device }
files-history-older = Older versions aren’t on this device yet
files-conflict-needs-review = Needs review
files-conflict-resolved-strategy = Resolved: { $strategy }
files-conflict-resolved = Resolved
files-strategy-keep-current = Keep current
files-strategy-use-other = Use other
files-strategy-keep-both = Keep both
files-strategy-merge = Merge
files-conflict-line = Conflict { $conflict }: { $count } { $count ->
    [one] version
   *[other] versions
}, { $state }
files-conflict-resolve-hint = Resolve from the context menu: keep current or use other
files-conflict-unsupported-hint = Keep both and Merge aren’t available for this file yet
files-history-title = Version history of { $name }
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
files-merge-review-menu = Review merge
files-merge-review-title = Review the merge of { $name }
files-merge-review-summary = No changes have been made yet
files-merge-review-checking = Comparing the two versions…
files-merge-review-checking-detail = Nothing changes while this runs.
files-merge-review-safe = Safe to merge
files-merge-review-needs-review = Needs review. No merge will be applied.
files-merge-review-unavailable = The versions can’t be compared right now. Refresh the file and compare them in Version history.
files-merge-review-no-apply = Keep both, or choose one after comparing them.
files-merge-review-incoming = Incoming revision from device { $device }
files-merge-review-diff = Version history shows both versions side by side.
files-merge-review-guard = If either version changes before you apply, nothing is merged.
files-merge-review-undo = You can restore either earlier version afterwards without losing later work.
files-merge-apply-safe = Apply safe merge
files-merge-apply-safe-detail = Applies only if both versions can still be kept whole.
files-conflict-resolved-signed = Conflict resolved
files-history-change-saved = History change saved
files-history-action-created = Created
files-history-action-edited = Edited
files-history-action-renamed = Renamed
files-history-action-moved = Moved
files-history-action-deleted = Deleted
files-history-action-conflict-resolved = Resolved conflict
files-history-action-restored = Restored
files-history-action-changed = Changed
files-history-current-version = { $action } (current version)
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
files-history-show-activity = Show in workspace activity
files-history-compare = Compare with current
files-history-hide-compare = Show version details
files-history-restore-selected = Restore this version
files-history-current-version-label = Current version

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
files-a11y-replica-unknown = Copies on other devices unknown
files-a11y-durably-replicated = Kept on other devices
files-a11y-not-durably-replicated = Not kept on other devices yet
files-a11y-shared-recipients = Shared with { $count } { $count ->
    [one] recipient
   *[other] recipients
}
files-a11y-version-conflict = Has a version conflict

## Service and received-share errors/status
files-open-terminal-failed = Could not open Terminal: { $error }
files-conflict-strategy-unsupported = That conflict strategy is not supported by Files yet
files-conflict-encode-failed = Could not encode the conflict request
files-conflict-request-too-large = This conflict can’t be resolved here. Try again from Version history.
files-conflict-resolve-failed = Could not resolve that conflict: { $error }
files-share-wrong-response = Share service returned the wrong response
files-share-unavailable-error = Share service unavailable: { $error }
files-share-encode-failed = Could not encode share request: { $error }
files-share-request-too-large = Too many people to share with at once. Share with fewer at a time.
files-share-request-failed = Could not request link: { $error }
files-share-flush-failed = Could not finish the share request: { $error }
files-share-no-response = Share service did not respond: { $error }
files-share-response-too-large = Share service returned an oversized response
files-share-response-truncated = Share response was truncated: { $error }
files-share-response-invalid = Invalid share-service response: { $error }
files-share-refused = Share service refused the request ({ $code })
files-received-refused = Received-file service refused the link ({ $code })
files-received-wrong-response = Received-file service returned the wrong response
files-received-encode-failed = Could not encode received-file request
files-received-request-too-large = This shared file can’t be received.
files-received-unavailable = Received-file service is unavailable
files-received-no-response = Received-file service did not respond
files-received-response-too-large = Received-file service returned an oversized response
files-received-response-truncated = Received-file service response was truncated
files-received-response-invalid = Received-file service returned an invalid response
files-received-not-available = Received-file service is not available
files-received-fd-clone-failed = The shared file couldn’t be received. Try again.
files-received-fd-invalid = The shared file couldn’t be received. Try again.
files-received-file = Received shared file
files-received-mount-starting = Shared file admitted; mount is starting
files-received-admitted = Shared file admitted
files-received-link-read-failed = Could not read the shared-file link
files-received-link-too-large = This link is too long to open.
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
files-show-as-grid = Show as grid
files-show-as-list = Show as list
files-location = Location
files-view-options = View options
files-path-placeholder = Type a path, e.g. ~/Documents
files-place-home = Home
files-place-favorites = Favorites
files-place-system = Locations
files-place-root = Root
files-place-trash = Trash
files-place-drives = Drives
files-drive-unnamed-internal = Drive in this computer
files-drive-unnamed-external = External drive
files-drive-internal = In this computer
files-drive-external = External drive
files-drive-open = Opened
files-drive-open-action = Open drive
files-drive-gone = This drive is no longer connected.
files-drive-open-failed = Could not open the drive: { $error }
files-trash-unavailable = Trash is unavailable on this device.
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
files-try-again = Try again
files-loading = Loading…
files-loading-folder = Loading { $path }
files-loading-title = Opening folder
files-loading-detail = Loading { $path }
files-directory-read-failed = Could not open this folder: { $error }
files-load-error-title = Couldn’t open “{ $name }”
files-folder-error-permission = You don’t have permission to see inside it.
files-folder-error-missing = It was moved or deleted.
files-open-enclosing = Open enclosing folder
files-a11y-load-error = Couldn’t open this folder: { $error }
files-a11y-panel-details = { $title }. { $summary }. { $details }. Press Escape to close
files-a11y-dialog-close = { $summary }. Press Escape to close
files-a11y-activity = Files activity
files-a11y-current-folder = Current folder: { $path }
files-a11y-places = Places
files-a11y-favourite = Favorite
files-a11y-system-location = System location
files-a11y-place-description = { $section }, { $path }
files-a11y-current-breadcrumb = Current folder, { $path }. Press Enter to edit the path
files-a11y-open-folder = Open { $path }
files-empty-trash = Trash is empty
files-empty-folder = This folder is empty
files-item-count = { $count } { $count ->
    [one] item
   *[other] items
}
files-selected-count = { $base }, { $count } selected
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
files-properties-title = Properties of { $name }
files-properties-summary = Filesystem metadata

## Ordinary operation results
files-new-folder-default-name = New folder
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
files-created-file = Created file
files-new-file-failed = New file failed: { $error }
files-added-favourite = Added to Favorites
files-removed-favourite = Removed from Favorites
files-moved-to-folder = Moved { $count } { $count ->
    [one] item
   *[other] items
} to “{ $destination }”
files-nothing-to-move = Nothing to move

## Context, view and sort menus
files-menu-open = Open
files-action-reveal = Reveal in Files
files-menu-restore = Restore
files-menu-delete-permanently = Delete permanently
files-menu-empty-trash = Empty Trash
files-menu-rename = Rename
files-menu-cut = Cut
files-menu-copy = Copy
files-menu-properties = Properties…
files-menu-paste = Paste
files-menu-new-folder = New folder
files-menu-new-file = New file…
files-menu-move-trash = Move to Trash
files-menu-add-favourite = Add to Favorites
files-menu-remove-favourite = Remove from Favorites
files-menu-hide-hidden = Hide hidden files
files-menu-show-hidden = Show hidden files
files-menu-sort-name = Sort by name
files-menu-sort-size = Sort by size
files-menu-sort-type = Sort by type
files-menu-sort-date = Sort by date
files-menu-open-terminal = Open terminal here
files-sort-name = Name
files-sort-size = Size
files-sort-type = Type
files-sort-date = Date
files-sort-ascending = Ascending
files-sort-descending = Descending
files-new-from-template = New { $name }
files-drag-items = { $count } items

## Archives
files-archive-extract = Extract archive…
files-archive-extract-here = Extract here
files-archive-compress = Compress to ZIP
files-archive-select-compress = Select files or folders to compress
files-archive-read-only = Archives are read-only until extracted
files-archive-select = Select a ZIP, TAR, or TAR.GZ archive to extract
files-archive-extracted =
    { $count ->
        [one] Extracted 1 file
       *[other] Extracted { $count } files
    }
files-archive-extracted-partial = Extracted { $count } files; { $failures }
files-archive-extract-failed = Could not extract archive: { $error }
files-archive-created = Created { $name }
files-archive-create-failed = Could not create archive: { $error }

## File-operation validation
files-folder-name-empty = Folder name cannot be empty
files-folder-name-slash = Folder name cannot contain '/'
files-name-empty = Name cannot be empty
files-name-slash = Name cannot contain '/'
files-no-parent = No parent directory
files-name-exists = “{ $name }” already exists
files-destination-inspect-failed = Cannot check “{ $path }”: { $error }
files-destination-names-exhausted = Too many files named “{ $name }” in “{ $path }”. Choose another name or folder.
files-source-no-name = Source has no file name
files-menu-send-nearby = Send to nearby device…
files-menu-run-shortcut = Run shortcut
files-shortcut-running = Running “{ $shortcut }”…
files-shortcut-started = “{ $shortcut }” is running
files-shortcut-failed = “{ $shortcut }” didn't run: { $reason }
files-action-send-nearby-device = Send to { $device }
files-nearby-send-to-device = Send to { $device }
files-nearby-select-one = Select one file or folder to send.
files-nearby-loading-devices = Looking for your devices…
files-nearby-no-devices = None of your devices are nearby.
files-nearby-choose-device = Choose a device.
files-nearby-queueing = Preparing to send…
files-nearby-accepted = Accepted.
files-nearby-declined = Declined.
files-nearby-cancelled = Sending cancelled.
files-nearby-retrying = Sending again…
files-nearby-accept = Accept
files-nearby-retry = Retry
files-nearby-consent-detail = Nothing is saved until you accept.
files-nearby-retry-detail = The file is still there, so sending can pick up where it stopped.
files-nearby-offer-title = Nearby Send from { $device }
files-nearby-offer-summary = Receive { $name }?
files-nearby-offer-size = Transfer size: { $bytes } bytes
files-nearby-failed-title = Nearby Send failed
files-nearby-failed-device = Device: { $device }
files-nearby-choose-destination = Choose where to save
files-nearby-no-destinations = There’s no folder this can be saved to.
files-nearby-destination-count = Choose from { $count } folders.
files-nearby-save-to-destination = Save to { $destination }
files-nearby-save-and-remember = Save to { $destination } and use it for this session

files-share-not-available = Sharing isn’t available on this device

files-name-invalid = Enter a valid file or folder name

## Open With and Recent
files-menu-open-with = Open with
files-open-with-other = Other application…
files-open-with-none = No application opens this file
files-open-with-default = { $app } (default)
files-open-with-default-badge = Default
files-open-with-once-group = Open once
files-open-with-always-group = Always use for { $type }
files-open-with-search = Choose an application for { $name }
files-open-with-app = Open with { $app }
files-open-with-always-app = Always open { $type } with { $app }
files-open-with-app-param = Application
files-open-with-always-param = Also make it the default for { $type }
files-open-with-now-default = { $app } now opens { $type }
files-open-with-unavailable = Choosing an application is unavailable here ({ $name })
files-open-with-not-a-candidate = { $app } does not open this file
files-place-recent = Recent
files-empty-recent = No recent files yet
files-recent-reveal = Open containing folder
files-recent-forget = Remove from Recent
files-recent-action-unavailable = This recent item is no longer available. Open Recent and choose a current item.
files-recent-not-a-folder = Recent is not a folder; open one to add files

## Cloud accounts
files-account-sign-in-title = Sign in to { $account }
files-account-sign-in-needed = { $service } at { $host } needs you to sign in again before this folder can sync.
files-account-sign-in-where = You can do that in Settings, under External accounts.
files-account-service-at-host = { $service } at { $host }
files-account-folder = Folder: { $path }
files-account-check-again = Check again
files-account-check-again-detail = Reads the account's state again and opens its folder once it is available.
files-action-send-airdrop = Send to a verified AirDrop lab receiver…
files-airdrop-unavailable = Verified AirDrop lab transfer is unavailable on this host.
files-airdrop-endpoint-invalid = Enter the receiver's literal IP address and a TLS port from 1 to 65535.
files-airdrop-source-unavailable = This file is no longer available.
files-airdrop-file-only = Choose one regular file for the AirDrop lab transfer.
files-airdrop-verifying = Waiting for receiver verification and permission to send this file.
files-airdrop-address = Receiver's literal IP address
files-airdrop-port = Receiver's TLS port
files-airdrop-action-detail = You confirm the receiver before this file is sent.

files-nearby-status = Transfer status
files-nearby-transfer-unavailable = This transfer is no longer available in this Files window.
files-nearby-status-detail = Read this transfer’s phase and confirmed byte counts.

files-clipboard-recovered = Recovered { $count } interrupted transfers. Recoverable copies are in Trash.
files-menu-recover-transfers = Recover interrupted transfers
files-clipboard-redone = Paste restored.

files-clipboard-cancelled = File transfer cancelled. Recover interrupted transfers before retrying.

files-invalid-destination-name = The destination must be one file name.

files-native-restore-select-one = Select one home Trash entry to restore. Other items are unchanged.

## History: what each change to Files did

file-explorer-add-bookmark = Added { $place } to Favorites
file-explorer-remove-bookmark = Removed { $place } from Favorites
file-explorer-new-folder = Created the folder { $name }
file-explorer-rename = Renamed { $entry } to { $name }
file-explorer-trash = Moved files to Trash
file-explorer-move = Moved files to { $to }
file-explorer-copy = Copied files to { $to }
file-explorer-revert = Took back a change to files
file-explorer-write-file = Wrote the file { $name }
file-explorer-compress = Compressed files into an archive

files-native-trash-delete-unavailable = These items can be restored. Permanent deletion is unavailable here.
files-user-trash-unavailable = Your Trash isn’t available in this workspace.

files-open-with-access-param = Access to the original file
files-open-with-launch-unavailable = This application cannot open the original file right now.

files-open-with-busy = Wait for the current file to open before opening another.
files-open-with-action-unavailable = Choose Open with again from the current file.
files-open-with-write-required = You need permission to edit the original file.
files-open-with-default-unavailable = Open this original file once without changing its default application.
files-open-with-access-unavailable = This application cannot use the selected access to the original file.
files-open-with-selection-changed = The selected file is no longer available. Choose it again.
files-open-with-timeout = Opening the original file took too long. Try again.

files-mounted-converted = Read-only converted cloud document
files-mounted-source = { $source }, a read-only converted view
files-mounted-readonly = Read-only conversion
files-mounted-open = Open read-only conversion
files-mounted-copy = Create independent editable copy…
files-mounted-independent = Edits to an independent local copy do not change the cloud original.
file-explorer-import-mounted = Created independent editable copies in { $folder }

## Notices
files-notice-undo = Undo
files-notice-open-with = Open With…
files-notice-dismiss = Dismiss
files-menu-undo = Undo
files-open-failed = Couldn’t open “{ $name }”
files-open-unavailable = Files can’t open documents in this window.
files-open-no-answer = The desktop didn’t respond.
files-new-folder-failed-title = Couldn’t create the folder
files-rename-failed-title = Couldn’t rename
files-trash-failed = Couldn’t move to Trash
files-move-failed = Couldn’t move everything to “{ $destination }”
files-paste-failed = { $count } of { $total } { $total ->
    [one] item
   *[other] items
} couldn’t be pasted
files-clipboard-partial = Couldn’t paste everything
files-error-permission = You don’t have permission.
files-error-not-found = It no longer exists.
files-error-busy = It’s in use.
files-error-exists = Something with that name already exists.
files-error-no-space = There isn’t enough space.
files-error-read-only = This location is read-only.
files-error-not-empty = The folder isn’t empty.
files-notice-cancel = Cancel
files-copying = Copying { $count } { $count ->
    [one] item
   *[other] items
} to “{ $folder }”
files-moving = Moving { $count } { $count ->
    [one] item
   *[other] items
} to “{ $folder }”
files-transfer-progress = { $done } of { $total }
files-transfer-stopped = Stopped
files-clash-title-one = “{ $name }” already exists in “{ $folder }”
files-clash-title-many = { $count } items already exist in “{ $folder }”
files-clash-detail = Replacing moves the existing items to Trash.
files-clash-skip = Skip
files-clash-keep-both = Keep both
files-clash-replace = Replace
files-preview-detail = { $what }, modified { $modified }
files-drop-nowhere = Open a folder to drop files into it.
