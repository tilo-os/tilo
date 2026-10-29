messages-empty-heading = Your people, together
messages-empty-hint = Add people in Contacts to start a conversation.
messages-open-contacts = Open Contacts
messages-open-contacts-description = Add the people you message in Contacts
messages-choose-heading = Choose a person
messages-choose-hint = Your conversations stay separate for each service and account.
messages-choose-conversation = Choose a conversation
messages-held-hint = Kept in this Messages. Words written here wait for a person to send them from a device holding the account.
messages-conversation-missing = This conversation is no longer here.
messages-people = People
messages-back = Back to people
messages-route = Message using
messages-tilo = Tilo
messages-whatsapp = WhatsApp
messages-signal = Signal
messages-sms = SMS
messages-no-account = Not configured
messages-no-recipient = Confirm a service address for this person.
messages-no-history = No messages on this service yet
messages-history-unavailable = This service's message history is unavailable.
messages-transport-unavailable = Messaging connection unavailable
messages-compose = Write a message
messages-send = Send
messages-route-label = { $service } from { $account } to { $recipient }
messages-account-label = { $service } from { $account }
messages-native-account = Your Tilo identity
messages-not-selected = Choose a service and account before writing a message.
messages-contact-missing = This contact is unavailable.
messages-association-conflict = This service address has conflicting contact associations. Review them in Contacts.
messages-recipient = Service recipient
messages-confirm-hint = Verify this address belongs to the person above. Similar names or phone numbers are not proof.

messages-identity-unavailable = A verified service identity is required before this contact can be connected.

messages-history-loading = Loading messages…
messages-refresh-history = Refresh messages
messages-incoming = Received
messages-outgoing = You
messages-submitted = Submitted
messages-server-acknowledged = Accepted by service
messages-delivered = Delivered to recipient
messages-read = Read by recipient
messages-uncertain = Send status uncertain
messages-load-older = Load older messages
messages-loading-older = Loading older messages…
messages-history-start = This is the start of the conversation.
messages-list-people = List people
messages-list-routes = List messaging choices

messages-send-uncertain = The outcome could not be confirmed. Check this conversation before sending again.

messages-send-invalid = Enter a message and select a current conversation.
messages-send-reserved = This send was already attempted. Check its status in the conversation.

messages-route-current = Message using { $route }

messages-list-account-states = List messaging account states
messages-account-unconfigured = Add this service account in Settings to connect it.
messages-account-unavailable = This account's messaging service is unavailable. Check its setup in Settings.
messages-account-linking = Linking this account… Confirm the request on your phone.
messages-account-locked = Unlock this account to use messaging.
messages-account-offline = This account is offline. Your conversation will resume when it reconnects.
messages-account-ready = Account ready
messages-account-needs-relink = Link this account again to resume messaging.
messages-account-failed = This account's connection stopped. Check its connection and account setup.

messages-account-unknown = Connection status is unavailable.

messages-setup-accounts = Accounts
messages-setup-select = Select the service and account to link on this device.
messages-start-link = Link this device
messages-finish-link = Finish linking
messages-cancel-link = Close setup
messages-setup-scan = Scan this private code from Linked devices on your phone, then finish linking.
messages-setup-code = Private device linking code for the selected account
messages-setup-unavailable = Private linking is unavailable for this account on this device.
messages-setup-active = This account already has a linking attempt. Finish or close it before starting another.
messages-setup-interrupted = Linking stopped. Return to account selection to start a new attempt.
messages-first-reset = Return to account selection
messages-setup-linked = Device linked. Messaging resumes after its connection and inbox are ready.


messages-first-signal = Link a Signal account
messages-first-self-review = Verify the selected service identity: { $identity }
messages-first-confirm = Confirm identity and request access
messages-first-permission-review = Review this account’s access request in Passwords. Messages starts only after you approve it.

messages-first-sms = Choose an SMS SIM
messages-select-sim = Use this SIM
messages-no-sims = No available SIMs were found on this device.
messages-select-sim-name = Use { $sim }

messages-first-whatsapp = Link a WhatsApp account
messages-whatsapp-show-code = Show private linking code
messages-whatsapp-review-storage = Review protected storage access
messages-whatsapp-refresh-code = Refresh private linking code
messages-whatsapp-storage-explanation = Review private encrypted storage access before linking. After linking, confirm the identity reported by WhatsApp. This version supports direct text and ordinary private group text, with reviewed membership changes. Community, disappearing-message, media and call features are unavailable.
messages-first-tilo = Use my Tilo identity
messages-setup-link-heading = Link an account
messages-setup-your-accounts = Your accounts
messages-setup-service-missing = Messaging isn't running here, so no account can be linked. Linking WhatsApp, Signal or a Tilo identity needs the messaging service a Tilo computer runs; a browser has none.
messages-first-whatsapp-hint = Scan a code with WhatsApp on your phone
messages-first-signal-hint = Scan a code with Signal on your phone
messages-first-tilo-hint = Message people who use Tilo, with no other account
messages-first-account-prepare = Set up a private Messages account

messages-list-groups = List private groups
messages-read-group-membership = Read group membership
messages-close-groups = Close groups
messages-groups = Private groups
messages-refresh-group-membership = Refresh membership
messages-account-groups = WhatsApp groups for { $account }
messages-groups-unavailable = Group membership is unavailable for this account.
messages-groups-empty = No participating groups.
messages-group-untitled = Untitled group
messages-groups-scope = These groups belong to the selected WhatsApp account, separately from workspace collaborators. Refresh membership before sending or changing members.
messages-group-role-member = Member
messages-group-role-admin = Administrator
messages-group-role-super-admin = Group creator

messages-group-history = Read group messages
messages-group-send = Send to this group
messages-group-add = Add reviewed contact
messages-group-remove = Remove reviewed member
messages-group-leave = Leave this group
messages-group-add-choice = Add { $person } using { $route }
messages-group-remove-choice = Remove { $member }

## History: what each change did, by verb and by field

# The conversations kept in the record.
messages-deliver-message = Delivered a message from { $from }
messages-send-message = Queued a message to { $to }
messages-reply = Queued a reply

# The people in Contacts and the service accounts Messages reaches them on.
messages-project-contact = Added { $item } to your contacts
messages-remove-contact = Removed the contact { $item }
messages-confirm-endpoint = Linked the { $service } address { $recipient } for { $item }
messages-confirm-account = Linked the { $service } account { $recipient } to { $item }
messages-replace-endpoint = Replaced the { $service } address for { $item } with { $recipient }
messages-replace-account = Replaced the { $service } account for { $item } with { $recipient }
messages-project-phones = Changed the phone numbers of { $item }
messages-project-emails = Changed the email addresses of { $item }
messages-project-contacts = Changed your contacts
messages-forget-endpoint = Unlinked an address from { $item }

# The service accounts a person adds, removes and switches between.
messages-publish-browser-identity = Added the account { $item } for use in this browser
messages-save-account = Saved the account { $item }
messages-publish-signal-account = Added the Signal account { $item }
messages-publish-sms-account = Added the SMS account { $item }
messages-publish-whatsapp-account = Added the WhatsApp account { $item }
messages-publish-tilo-account = Added the Tilo account { $item }
messages-remove-account = Removed the account { $item }
messages-set-active-account = Switched to another account

# The drafts a person has not sent yet.
messages-keep-draft = Kept a draft
messages-drop-draft = Discarded a draft

# A field of an entry, and what the entry is called: the words say what
# changed it, so they read on their own.
messages-message-from = A message from { $item }
messages-message-to = Who a message is for
messages-message-body = What a message says
messages-message-date = When a message was sent
messages-message-at-ms = When a message was sent
messages-message-outgoing = Whether a message was written here
messages-message-queued = Whether a message is waiting to be sent
messages-person-label = The name of { $item }
messages-person-removed = Whether { $item } is in your contacts
messages-person-phones = The phone numbers of { $item }
messages-person-emails = The email addresses of { $item }
messages-person-photo = The photo of { $item }
messages-endpoint-service = The service { $item } is reached on
messages-endpoint-recipient = The address { $item } is reached at
messages-endpoint-scope = The account { $item } is reached through
messages-endpoint-revision = The linked identity of { $item }
messages-endpoint-removed = Whether { $item } is still linked
messages-person-account-type-id = The kind of the account { $item }
messages-person-account-display-name = The name of the account { $item }
messages-person-account-config = The connection settings of { $item }
messages-person-account-enabled = Whether { $item } is in use
messages-person-account-tint = The colour of { $item }
messages-person-accounts-active = The account in use for a service
messages-message-drafts-drafts = The unsent words kept for each conversation

messages-conversation-with = Conversation with { $person }
