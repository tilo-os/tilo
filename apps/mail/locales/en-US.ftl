# Mail — user-facing strings.
#
# Keys are added as surfaces are migrated off literals; a string that is still
# a literal in the source is not translated yet, not intentionally excluded.

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

mail-account-secret-unavailable = This account's password could not be opened on this device. Check the account in Settings, under External Accounts.
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
mail-status-send-failed = Send failed: { $error }
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
