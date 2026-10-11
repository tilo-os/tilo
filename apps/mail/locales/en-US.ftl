# Mail — user-facing strings.
#
# Keys are added as surfaces are migrated off literals; a string that is still
# a literal in the source is not translated yet, not intentionally excluded.

## Account sheet
mail-setup-heading-add = Add account
mail-setup-heading-edit = Account settings
mail-setup-label-display-name = Name
mail-setup-label-email = Email
mail-setup-label-username = Username
mail-setup-label-password = Password
mail-setup-label-imap-host = Incoming server
mail-setup-label-smtp-host = Outgoing server
mail-setup-label-sign-in = Sign in with
mail-setup-auth-password = Password
mail-setup-auth-oauth = Browser
# { $provider } is the provider's own name (Gmail, Outlook, …).
mail-setup-oauth-sign-in = Sign in with { $provider }
mail-setup-oauth-provider-fallback = your provider
mail-setup-oauth-note-unavailable = Browser sign-in unavailable
mail-setup-tls = TLS
mail-setup-advanced = Advanced
mail-setup-save = Save
mail-setup-verifying = Checking…
mail-setup-finding-provider = Finding servers…
mail-setup-cancel = Cancel
mail-setup-remove = Remove account

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
mail-contact-recipients = Contacts recipients
mail-contact-draft-choice = Draft to { $person } ({ $id }) at { $email } from { $sender } ({ $sender_account })
mail-contact-draft-prepared = Draft ready
mail-contact-sender-changed = Sending account changed
mail-contact-prepare = Prepare draft
mail-contact-existing-draft = A draft is already open
mail-contact-unavailable = Shared Contacts is unavailable
mail-contact-person-unavailable = Contact is no longer available
mail-contact-recipient-changed = Contact changed; select and review it again
mail-contact-account-changed = Sending account changed; select and review it again
