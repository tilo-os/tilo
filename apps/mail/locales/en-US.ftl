# Mail — user-facing strings.
#
# Keys are added as surfaces are migrated off literals; a string that is still
# a literal in the source is not translated yet, not intentionally excluded.

## First run
mail-first-run-title = No email account yet
mail-first-run-hint = Add an email account in Settings, under External accounts.
mail-first-run-action = Open External accounts

## The empty message list: what is not here, and the account its mail would
## come from while none has arrived. { $folder } is the folder's own name.
mail-zero-nothing-yet = Nothing has arrived yet
mail-zero-account-settings = Open account settings
mail-zero-folder = No messages in { $folder }

## Compose window
mail-compose-label-to = To
mail-compose-label-cc = Cc
mail-compose-label-bcc = Bcc
mail-compose-label-subject = Subject
mail-compose-placeholder-to = Name or email
mail-compose-cc-bcc = Cc/Bcc
mail-compose-send = Send
mail-compose-save-draft = Save draft
mail-compose-discard = Discard
# The link prompt above the buttons.
mail-compose-prompt-link = Link
mail-compose-prompt-add = Add link
mail-compose-prompt-cancel = Cancel

# Shared address-book choices. Hints are selected explicitly, never identity proof.
mail-contact-recipients = Contacts
mail-contact-write = Write to a contact
mail-contact-draft-choice = Draft to { $person } ({ $id }) at { $email } from { $sender } ({ $sender_account })
mail-contact-draft-prepared = Draft ready
mail-contact-sender-changed = Sending account changed
mail-contact-prepare = Prepare draft
mail-contact-existing-draft = A draft is already open
mail-contact-unavailable = Shared Contacts is unavailable
mail-contact-recipient-changed = Contact changed; select and review it again
mail-contact-account-changed = Sending account changed; select and review it again

## History: what each change did, by field and by verb

mail-mark-read = Marked messages { $read ->
    [true] read
   *[false] unread
}
mail-archive = Archived messages
mail-trash = Moved messages to Trash
mail-move-messages = Moved messages to { $to }
mail-purge = Deleted messages for good
mail-ingest = Fetched mail
mail-server-applied = The mail server took a change
mail-keep-copies = Kept fetched mail for your other devices
mail-forget-copies = Dropped mail its server no longer holds
mail-mail-copies-copies = Changed the mail kept for your other devices
mail-start-draft-body = Started writing a message
mail-discard-draft = Discarded the draft
mail-send-message = Sent a message from { $account }
mail-cancel-send = Cancelled sending a message
mail-message-state-starred = { $new ->
    [true] Starred a message
   *[false] Unstarred a message
}
mail-message-state-folder = Moved a message to { $new }
mail-message-state-labels = Changed the labels of a message
mail-local-folder-name = Renamed the folder { $item }
mail-mail-folders = Changed the folders
mail-mail-messages = Changed the mailbox
mail-mail-outbox = Recorded how a sent message went
mail-draft-state-sender = Chose who the draft is sent from
mail-draft-state-to = Changed who the draft goes to
mail-draft-state-cc = Changed who the draft is copied to
mail-draft-state-bcc = Changed who the draft is blind copied to
mail-draft-state-subject = Changed the subject of the draft
mail-draft-state-body = Edited the draft
mail-draft-state-html-body = Edited the draft
mail-draft-state-calendar-ics = Changed the invitation in the draft
mail-draft-state-attachments = Changed the attachments of the draft
mail-draft-state-rich-body = Edited the draft
mail-save-draft = Saved the draft
mail-attach = Attached { $name } to the draft
mail-mail-server = Recorded how a mail server took a change
mail-mail-held = Changed the mail held in the mailbox
mail-held-message-message-id = Changed the Message-ID of a held message
mail-held-message-from = Changed the sender of a held message
mail-held-message-to = Changed the recipients of a held message
mail-held-message-cc = Changed who a held message is copied to
mail-held-message-subject = Changed the subject of a held message
mail-held-message-body = Changed the text of a held message
mail-held-message-minutes = Changed when a held message was sent
mail-held-message-in-reply-to = Changed the message a held message answers
mail-held-message-conversation = Moved a held message to another conversation
mail-deliver-message = Delivered { $subject }
mail-draft-message = Drafted { $subject }
mail-draft-reply = Drafted a reply

mail-forward-not-downloaded = { $count ->
    [one] { $files } is not downloaded yet, so it is not attached
   *[other] { $files } are not downloaded yet, so they are not attached
}
mail-forward-too-large = { $count ->
    [one] { $files } is more than one message can carry, so it is not attached
   *[other] { $files } are more than one message can carry, so they are not attached
}

mail-account-secret-unavailable = This account's password could not be opened on this device. Check the account in Settings, under External accounts.
mail-import-sign-in-required = Imported account settings require authentication. Sign in before connecting.

## Reader: an HTML message's remote content, links and format
mail-remote-blocked = Remote images aren’t loaded
mail-remote-load = Load images
mail-remote-always = Always for this sender
mail-remote-loaded = Remote images are loaded for this message
mail-remote-sender-loads = Remote images from this sender always load
mail-remote-stop = Stop loading them
mail-link-open = Open link
mail-link-copy = Copy link address
mail-copy = Copy
mail-select-all = Select all
mail-show-quoted = Show quoted text
mail-show-plain = Show plain text
mail-show-formatted = Show formatted

## What the window and its parts are called for assistive technology

mail-title = Mail
mail-a11y-queued-message = Queued message
mail-a11y-outbox = Outbox
mail-a11y-outbox-empty = Nothing is waiting to be sent
mail-a11y-no-message = No message selected
mail-a11y-message-actions = Message actions
mail-a11y-bulk-select = Select messages
mail-a11y-bulk-done = Done selecting messages
mail-a11y-thread-messages = { $count ->
    [one] { $count } message in this conversation
   *[other] { $count } messages in this conversation
}
mail-a11y-preview-unread = Preview, still unread
mail-a11y-download-first = Download this attachment before saving it
mail-a11y-syncing = Syncing mail
mail-compose-placeholder-body = Write your message
mail-compose-a11y = Compose email
mail-compose-a11y-description = Email composer
mail-shortcuts-title = Keyboard shortcuts

## Window and accessibility copy
mail-a11y-mailboxes = Mailboxes

## What the window shows
mail-view-drawer = Folder drawer
mail-view-reading = Reading pane
mail-view-list = Message list
mail-view-all = Folders, message list, and reading pane

## Facts, property labels and section titles
mail-a11y-current-view = Current view: { $view }
mail-a11y-reading = Reading: { $subject }
mail-a11y-save-attachment = Save attachment { $file } ({ $size })
mail-a11y-refused = The server refused this message: { $reason }
mail-a11y-cancel-sending = Cancel sending to { $to }
mail-a11y-outbox-row = To { $to }: { $subject }
mail-a11y-no-messages = No messages in { $folder }
mail-status-send-failed = Couldn’t send the message: { $error }
mail-purge-body = { $count ->
    [one] One message in Trash will be deleted from this mailbox and the server. This can’t be undone.
   *[other] { $count } messages in Trash will be deleted from this mailbox and the server. This can’t be undone.
}
mail-status-attach-failed = Couldn’t attach { $name }: { $error }
mail-status-attached = Attached { $name }

mail-status-picker-failed = Couldn’t open attachment picker: { $error }

mail-status-archived = { $count ->
    [one] Archived one message
   *[other] Archived { $count } messages
}
mail-status-trashed = { $count ->
    [one] Moved one message to Trash
   *[other] Moved { $count } messages to Trash
}
mail-status-marked-read = { $count ->
    [one] Marked one message read
   *[other] Marked { $count } messages read
}
mail-status-marked-unread = { $count ->
    [one] Marked one message unread
   *[other] Marked { $count } messages unread
}
mail-status-deleted = { $count ->
    [one] Deleted one message
   *[other] Deleted { $count } messages
}
mail-status-trusted-senders-not-saved = Couldn’t save your trusted senders: { $error }
mail-status-account-not-saved = Selected account, but could not save that preference: { $error }
mail-status-purged = { $count ->
    [one] Permanently deleted one trashed message
   *[other] Permanently deleted { $count } trashed messages
}
mail-status-purge-nothing = Nothing was deleted; the selected messages changed before confirmation
mail-status-preferences-not-saved = Could not save your Mail preferences: { $error }

mail-from = From { $sender }
mail-status-copy-cancelled = Attachment save cancelled
mail-status-copy-saved = Saved a copy as { $file }
mail-status-selection-cancelled = Attachment selection cancelled
mail-status-selection-composer-closed = Attachment selection cancelled because the composer was closed
mail-status-picker-disconnected = Attachment picker disconnected before selecting a file
mail-status-choose-file = Choose a file to attach

## What the window shows
mail-attachment-not-downloaded = Attachment content is not downloaded yet
mail-draft-subject = Draft: { $subject }
mail-not-sent-yet = This message has not been sent yet.
mail-no-subject = No subject
mail-to = To { $recipient }
mail-preview = Preview
mail-preview-unread = Preview, still unread

mail-row-name = { $sender }: { $subject }
mail-list-name = { $folder } messages
mail-selected-count = { $count } selected
mail-bulk-select-all = Select all visible messages
mail-bulk-archive = Archive selected messages
mail-bulk-read = Mark selected messages read
mail-bulk-unread = Mark selected messages unread
mail-bulk-purge = Delete selected trashed messages permanently
mail-bulk-delete = Move selected messages to Trash
mail-draft-subject-to = Draft: { $subject }, to { $to }

## What the folders are called
mail-folder-inbox = Inbox
mail-folder-starred = Starred
mail-folder-sent = Sent
mail-folder-drafts = Drafts
mail-folder-archive = Archive
mail-folder-trash = Trash
mail-folder-junk = Junk
mail-refresh-offline = Cannot refresh { $folder } for { $account } while IMAP is offline

## The window bar's slots
mail-bulk-done = Done
mail-delete = Delete
mail-delete-permanently = Delete permanently
mail-reply = Reply
mail-reply-all = Reply all
mail-forward = Forward
mail-move-to-inbox = Move to Inbox
mail-star = Star
mail-add-account = Add account
mail-new-message = New message
mail-cancel-sending = Cancel sending
mail-bar-discard-draft = Discard draft
mail-close-keep-draft = Close and keep in Drafts

## Attaching a file, and saving a received one
mail-compose-attach-file = Attach file
mail-save-attachment = Save attachment

## Actions

mail-effects-compose = Compose message
mail-effects-contact-recipients = Choose a recipient
mail-effects-account-settings = Account settings
mail-effects-prepare-contact-draft = Draft to a contact
mail-effects-refresh = Refresh folder
mail-effects-prepare-calendar-invitation = Prepare calendar invitation
mail-effects-open = Open
mail-effects-read-message = Read message
mail-effects-load-remote-content = Load remote images
mail-effects-always-load-remote-content = Always load remote images
mail-effects-search-messages = Search messages
mail-effects-draft-reply = Draft reply
mail-effects-open-attachment = Open attachment
mail-effects-content = Get content
mail-effects-export-content = Export content
mail-mailbox = Mail
mail-mailbox-mark-read = Mark read
mail-mailbox-trash = Move to Trash
mail-mailbox-move-messages = Move messages
mail-mailbox-ingest = Add fetched messages
mail-mailbox-server-applied = Server applied the update
mail-mailbox-start-draft-body = Start the draft body
mail-mailbox-create-draft = New draft
mail-mailbox-show-draft = Show the draft
mail-mailbox-attach = Attach to draft
mail-mailbox-send-message = Send message
mail-mailbox-deliver-message = Deliver message
mail-mailbox-draft-message = Draft message
mail-trusted-senders = Trusted senders
mail-trusted-senders-trust = Trust this sender
mail-trusted-senders-distrust = Stop trusting this sender

## What the app is called in a window and in the launcher
mail-app-title = Mail
## What the records are called, and the actions on them
mail-copies = Mail on your devices
mail-copies-keep-copies = Keep fetched mail
mail-copies-forget-copies = Forget fetched mail

## What Mail says once: what it did, what it could not do, and what is next
mail-notice-server-unreachable = Mail couldn’t reach the mail server
mail-notice-offline = You’re offline. New mail arrives when you’re back online.
mail-notice-sign-in-failed = Can’t sign in. Check the account’s password in Settings, under External accounts.
mail-notice-sign-in-failed-account = Can’t sign in to { $account }. Check its password in Settings, under External accounts.
mail-notice-check-failed = Can’t check mail: { $error }
mail-notice-check-failed-account = Can’t check mail for { $account }: { $error }
mail-notice-sign-in-unfinished = Sign-in didn’t finish: { $error }
mail-notice-server-failed = Couldn’t update the mail server: { $error }
mail-notify-arrived = { $count ->
    [one] New message
   *[other] { $count } new messages
}
mail-outbox-waiting = Waiting to be sent

mail-status-picker-open = The file picker is already open
mail-status-select-before-purge = Nothing is selected to delete permanently
mail-status-purge-needs-trash = Only messages in Trash can be deleted permanently
mail-status-message-gone = That message is no longer here
mail-status-no-sending-account = Add a sending account before sending this message
mail-status-sending-unavailable = Sending isn’t available here; save the draft instead
mail-status-no-recipient = Add a recipient before sending
mail-status-sending = Sending…
mail-status-sent = Message sent
mail-status-draft-gone = That draft is gone
mail-status-invitation-drafted = Invitation drafted
mail-status-outbox-cancelled = Taken out of the outbox; nothing was sent
mail-status-moved = Moved one message to { $folder }
mail-status-selected-all = Selected all { $count } messages in { $folder }
mail-status-selected-all-inboxes = Selected all { $count } messages in every inbox
mail-status-reading-folder = { $count ->
    [one] Still reading this folder; one message selected so far
   *[other] Still reading this folder; { $count } selected so far
}
mail-purge-title = { $count ->
    [one] Delete this message permanently?
   *[other] Delete { $count } messages permanently?
}
mail-cancel = Cancel
mail-undo = Undo

## The host's own file and folder choosers
mail-attach-dialog-title = Attach file
mail-attach-dialog-accept = Attach
mail-attachment-save-accept = Save

## An attachment that cannot leave Mail
mail-attachment-unknown-message = That message is no longer in this mailbox
mail-attachment-not-loaded = That attachment is no longer in this mailbox
mail-attachment-not-kept = { $file } is no longer kept on this device
mail-attachment-too-large-save = { $file } is too large to save here; open it in Files instead
mail-attachment-changed = That attachment changed since it was downloaded
