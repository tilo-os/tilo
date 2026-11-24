# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-passwords = Passwords

d-so-tilo-passwords-keywords = password manager, password, passwords, credentials, logins, vault, keychain, secrets, passkeys, 1password, bitwarden, keepass, keepassxc, lastpass, passwort, passwörter, passwortmanager

d-oe75ekol = Item
    .doc = One saved secret: what it signs in to, who it signs in as and what it takes.
    .noun = { $count ->
        [one] item
       *[other] items
    }
    .untitled = Untitled item

d-ildzcyo2 = Title
    .doc = What it is called: a site, a network, a service.

d-ujkdowq4 = Kind

d-tpqopgvl = Username
    .doc = What identifies the person to the account: a username, an account, a network's name.

d-txdg33mm = Password
    .doc = The secret itself.

d-fxvfkz3x = Notes
    .doc = Anything worth keeping beside it, in the person's own words.

d-qgllbsrc = Origins
    .doc = The addresses it signs in to, each one a web address.

d-qoa35hei = Tags

d-japk242o = Fields
    .doc = Fields the person added, by their label: a PIN, an account number.

d-tzeujwn7 = Favorite

d-qzpekkx6 = Old
    .doc = Whether the password was last changed more than a year ago, as the daily check writes it.

d-oe75ekol-filter-favorite = Favorites

d-oe75ekol-filter-kind = Kind

d-oe75ekol-filter-tags = Tags

d-njro7d44 = Import row
    .doc = A row a browser's CSV held, waiting for the person to say whether it comes into the vault.
    .noun = { $count ->
        [one] import row
       *[other] import rows
    }

d-l7jn4eol = Title

d-s6pvezan = Username

d-4jcknnmq = Origin

d-jtrf57qd = Password

d-sx76etl2 = Notes

d-23s6djq3 = Skip
    .doc = Whether the vault already holds that login, so importing skips it.

d-njro7d44-filter-skip = Skip

d-iivmujua = Vault
    .doc = The workspace's vault: every secret its members share, and what the window learned from the device of what it tried to do.
    .noun = { $count ->
        [one] vault
       *[other] vaults
    }

d-vflzrccj = Items

d-6n7bahst = Converted
    .doc = The ids of device-vault items already taken into this document, so a second opening takes none of them twice.

d-wa4uxojt = Pending
    .doc = The rows the person is deciding about, before an import.

d-3xf6uda7 = Notice
    .doc = What the last import, export or backup did, as a line the transfer page reads.

d-iivmujua-filter-converted = Converted

d-k55olbaw = Login list
    .doc = What a list of logins says: each by the item that holds it, and its name.
    .noun = { $count ->
        [one] login list
       *[other] login lists
    }

d-nkbtq6y2 = Logins

d-du4xkpjn = Login entry
    .doc = One login in such a list.
    .noun = { $count ->
        [one] login entry
       *[other] login entries
    }

d-mtdotryg = Item

d-yiseqxtu = Name

d-e7tbzvws = Asked item
    .doc = An item as the device's engines read and write it.
    .noun = { $count ->
        [one] asked item
       *[other] asked items
    }

d-rzuvlal6 = Title

d-65lzjom4 = Kind

d-gtezt3ys = Username

d-nqzlkgho = Password

d-nyoijvnx = Notes

d-pupxu7gb = Origins

d-zrtsugqu = Tags

d-yczcbynv = Fields

d-uynlhftv = Items ask
    .doc = What the export engines are asked: the vault's items.
    .noun = { $count ->
        [one] items ask
       *[other] items asks
    }

d-6nicnbs2 = Items

d-7vsmhup7 = Backup ask
    .doc = What the backup engine is asked: the vault's items under a password.
    .noun = { $count ->
        [one] backup ask
       *[other] backup asks
    }

d-a45t5mnl = Items

d-wmhsd5ch = Passphrase

d-okwle4am = Restore ask
    .doc = What the restore engine is asked: where the backup is, and the password it takes.
    .noun = { $count ->
        [one] restore ask
       *[other] restore asks
    }

d-rnonlkte = Url

d-cynjyelt = Passphrase

d-w5ifp6gt = Asked file
    .doc = What the CSV engine is asked: the file the person chose.
    .noun = { $count ->
        [one] asked file
       *[other] asked files
    }

d-roquvgtj = Url

d-awblwezm = Remove ask
    .doc = What the device's vault is asked to forget: the ids of the entries the vault took in.
    .noun = { $count ->
        [one] remove ask
       *[other] remove asks
    }

d-eevmellb = Ids

d-nkhxa77u = Device item
    .doc = One item as a device or a backup hands it over.
    .noun = { $count ->
        [one] device item
       *[other] device items
    }

d-wnjsdr3d = Id

d-lwvhidtr = Title

d-vd3jnhgx = Kind

d-f7f2gp2a = Username

d-6okrlhly = Password

d-p4viod5a = Notes

d-4ejpoi2g = Origins

d-wze5ieza = Tags

d-gr5iqwm5 = Fields

d-chdzlb3g = Device answer
    .doc = What the device's vault answers: its shareable entries, or why it could not be read.
    .noun = { $count ->
        [one] device answer
       *[other] device answers
    }

d-vqmxqaz7 = Items

d-is77ciid = Error

d-mdx37iwu = Csv row
    .doc = One row of a browser's CSV.
    .noun = { $count ->
        [one] csv row
       *[other] csv rows
    }

d-ufq2o5qa = Title

d-pjl3fx3t = Username

d-7ca4ejcb = Origin

d-u3j7epmt = Password

d-we5a77lw = Notes

d-cnaonhif = Rows answer
    .doc = The rows a CSV holds.
    .noun = { $count ->
        [one] rows answer
       *[other] rows answers
    }

d-janzxbln = Rows

d-u4mryepl = Url answer
    .doc = Where an engine kept what it made.
    .noun = { $count ->
        [one] url answer
       *[other] url answers
    }

d-vlz4ie4r = Url

d-5p7rahwe = Kind
    .doc = What kind of secret an item is: what it is for, which sets the fields it has and the words that name them.

d-qjsinpt7 = A login: a username and a password for a site
    .doc = A login: a username and a password for a site.

d-gajmqxvy = A key an API takes
    .doc = A key an API takes.

d-kjshvwsh = A note kept as a secret, with no address
    .doc = A note kept as a secret, with no address.

d-rq6bb2yq = The password of a Wi-Fi network
    .doc = The password of a Wi-Fi network.

d-lngptrz6 = Any other secret: a licence code, a recovery key
    .doc = Any other secret: a licence code, a recovery key.

d-734is3qi = New item
    .doc = Starts an item: it opens in the window, named by what is typed first.
    .did = added an item

d-734is3qi-title = Title

d-k6wf47xc = Save a login
    .doc = Adds a login in one step: what an agent, a script or the VM's own end-to-end check asks of the vault.
    .did = saved a login
    .w-a-login-needs-a-name = A login needs a name
    .w-a-login-needs-its-password = A login needs its password
    .w-is-not-a-web-address-such = { $trim } is not a web address, such as https://example.com

d-k6wf47xc-name = Name

d-k6wf47xc-origin = Origin

d-k6wf47xc-username = Username

d-k6wf47xc-password = Password

d-djkahzyt = List logins
    .doc = The vault's logins, by name and by the item that holds each: what a script, an agent or another device's window lists.

d-hk7yj34l = Save item
    .doc = Writes what the editor holds onto an item: the password and notes it was given, and the custom fields it typed.
    .did = saved an item
    .w-write-a-custom-field-as-label = Write a custom field as label = value
    .w-add-a-name-so-you-can = Add a name so you can find this item
    .w-add-something-to-the-note = Add something to the note
    .w-add-the = Add the { $lower }

d-hk7yj34l-password = Password

d-hk7yj34l-notes = Notes

d-hk7yj34l-fields = Fields

d-hopdjwb6 = Generate strong password
    .doc = Draws a strong password for the item: 20 characters, at least one capital, one small letter, one digit and one symbol.
    .did = generated a password

d-rb5tewbt = Remove from favorites
    .doc = Puts an item in the person's favorites, or takes it out.
    .w-remove-from-favorites = Remove from favorites
    .w-add-to-favorites = Add to favorites

d-rb5tewbt-favorite = Favorite

d-h4h5hsoo = Add a tag
    .doc = Gives an item a tag, as the words beside its tags are typed.
    .w-a-tag-needs-a-name = A tag needs a name

d-h4h5hsoo-tag = Tag

d-3dx4s6bc = Remove the tag
    .doc = Takes a tag off an item.

d-3dx4s6bc-tag = Tag

d-tma33s6p = Add a web address
    .doc = Gives an item an address it signs in to, checked as a web address, as typed beside them.
    .w-add-the-address = Add the address
    .w-is-not-a-web-address-such = { $trim } is not a web address, such as https://example.com

d-tma33s6p-origin = Origin

d-3aqm4fli = Remove the address
    .doc = Takes an address off an item.

d-3aqm4fli-origin = Origin

d-qocbfwqn = Copy secret
    .doc = Copies the item's secret to the person's clipboard.

d-tlxdhz4k = Copy identity
    .doc = Copies what identifies the person to the account: a login's username, an API key's account, a Wi-Fi network's name.

d-ypducoyo = Copy web addresses
    .doc = Copies the addresses the item signs in to, one a line.

d-qoybllz7 = Delete
    .doc = Deletes an item from the vault.
    .did = deleted an item

d-qoybllz7-item = Item

d-r7cooy7o = Check the password's age
    .doc = Notes an item whose password was last changed more than a year ago, as the daily check runs.

d-lxclcx4k = Write the addresses
    .doc = Replaces an item's addresses with `values`, one change.

d-lxclcx4k-values = Values

d-45ex6uaa = Write the tags
    .doc = Replaces an item's tags with `values`, one change.

d-45ex6uaa-values = Values

d-dnl6oesw = Write the custom fields
    .doc = Replaces an item's custom fields with `values`, one change.

d-dnl6oesw-values = Values

d-zzxcwuwh = Import from CSV
    .doc = Reads the browser's CSV the person chose into the vault's waiting rows, each marked when the vault already holds that login.

d-zzxcwuwh-file = File

d-ke7xdrrc = Import the logins
    .doc = Brings the rows the person agreed to into the vault, skipping the ones the vault holds.

d-l3duo5gz = Cancel the import
    .doc = Puts the waiting rows away.

d-swlh6e2h = Clear the waiting rows
    .doc = Empties the vault's waiting rows.

d-hnneai2d = Export to CSV
    .doc = The vault as a browser's CSV, named for what it holds: the person chooses where it goes.

d-t2p5z5f6 = Back up the vault
    .doc = The vault as an encrypted backup under `password`: the person chooses where it goes.
    .w-use-at-least-12-characters-for = Use at least 12 characters for the backup password

d-t2p5z5f6-password = Password

d-wzx4ymwg = Restore from a backup
    .doc = Brings the items a backup holds into the vault, and answers how many came in.
    .w-use-at-least-12-characters-for = Use at least 12 characters for the backup password

d-wzx4ymwg-password = Password

d-wzx4ymwg-file = File

d-flkitlvk = Import this device's vault
    .doc = Takes into the vault what this device's secret service holds and a workspace may share.

d-ugwe3ulv = Save login~present

d-d2bzao4x = Save editor~present

d-z623qzrl = Favorite~present

d-outbbn5k = Add tag~present

d-7y4ug253 = Add origin~present

d-bmtl4ylm = Vault window
    .w-favorites = Favorites
    .w-logins = Logins
    .w-api-keys = API keys
    .w-secure-notes = Secure notes
    .w-wi-fi = Wi-Fi
    .w-other-secrets = Other secrets
    .w-password-review = Password review
    .w-import-and-export = Import and export
    .w-all-items = All items
    .w-passwords = Passwords
    .w-tools = Tools
    .w-file = File
    .w-import-from-csv = Import from CSV…
    .w-export-to-csv = Export to CSV…
    .w-back-up-vault = Back up vault…
    .w-restore-from-backup = Restore from backup…
    .w-import-this-device-s-vault = Import this device's vault
    .w-new-item = New item
    .w-the-tool-you-chose-is-in = The tool you chose is in the content beside the list.
    .w-your-vault-is-empty = Your vault is empty
    .w-no-favorites-yet = No favorites yet
    .w-nothing-of-this-kind-yet = Nothing of this kind yet
    .w-encrypted-backup = Encrypted backup
    .w-a-backup-keeps-every-item-note = A backup keeps every item, note and custom field, encrypted with a password you choose.
    .w-browser-passwords = Browser passwords
    .w-move-logins-between-passwords-and-a = Move logins between Passwords and a web browser with a CSV file.
    .w-this-device-s-vault = This device's vault
    .w-logins-this-device-kept-before-the = Logins this device kept before the vault was shared come into the workspace once. The authentication material of this machine and its Wi-Fi networks stay on it.
    .w-edit-item = Edit item
    .w-save = Save
    .w-cancel = Cancel
    .w-kind = Kind
    .w-name = Name
    .w-what-it-is-called = What it is called
    .w-who-it-signs-in-as = Who it signs in as
    .w-leave-it-empty-to-keep-the = Leave it empty to keep the current one
    .w-the = The { $lower }
    .w-generate-strong-password = Generate strong password
    .w-notes = Notes
    .w-anything-worth-keeping = Anything worth keeping
    .w-network = Network
    .w-tags = Tags
    .w-add-a-tag = Add a tag
    .w-remove-the-address = Remove the address
    .w-add-a-web-address = Add a web address
    .w-custom-fields = Custom fields
    .w-one-a-line-as-pin-1234 = One a line, as PIN = 1234
    .w-pin-1234 = PIN = 1234
    .w-remove-from-favorites = Remove from favorites
    .w-add-to-favorites = Add to favorites
    .w-edit-details = Edit details
    .w-hidden-until-you-edit-the-item = Hidden until you edit the item
    .w-kept = { $len } kept
    .w-last-changed = Last changed
    .w-copy = Copy { $lower }
    .w-copy-web-addresses = Copy web addresses
    .w-hide = Hide
    .w-reveal-secret = Reveal secret
    .w-change-the-password = Change the password
    .w-open-the-note = Open the note
    .w-no-tags-yet = No tags yet.
    .w-delete-item = Delete item
    .w-nothing-saved-yet = Nothing saved yet
    .w-no-item-selected = No item selected
    .w-logins-secure-notes-wi-fi-passwords = Logins, secure notes, Wi-Fi passwords and API keys you save are kept here.
    .w-reveal-the-of = Reveal the { $lower } of “{ $title }”?
    .w-reveal = Reveal
    .w-it-shows-for-10-seconds-then = It shows for 10 seconds, then hides again.
    .w-delete = Delete “{ $title }”?
    .w-the-item-is-removed-from-the = The item is removed from the vault and cannot be restored.
    .w-change-the-of = Change the { $lower } of “{ $title }”
    .w-keep = Keep
    .w-the-password = The password
    .w-write-the-note-of = Write the note of “{ $title }”
    .w-the-note = The note
    .w-back-up-vault-2 = Back up vault
    .w-choose-file = Choose file…
    .w-backup-password = Backup password
    .w-use-at-least-12-characters = Use at least 12 characters
    .w-confirm-backup-password = Confirm backup password
    .w-the-passwords-do-not-match = The passwords do not match
    .w-restore-from-backup-2 = Restore from backup

d-p4ndizd3 = Review page
    .w-password-review = Password review
    .w-reused = Reused
    .w-weak = Weak
    .w-not-changed-in-a-year = Not changed in a year
    .w-nothing-to-check-yet = Nothing to check yet
    .w-save-a-password-and-the-review = Save a password and the review checks it for reuse, weakness and age.
    .w-nothing-to-fix = Nothing to fix
    .w-every-saved-password-is-unique-strong = Every saved password is unique, strong and recent.
    .w-needs-attention = Needs attention
    .w-about-this-review = About this review
    .w-passwords-checked = Passwords checked
    .w-checked-against = Checked against
    .w-only-the-passwords-in-this-vault = Only the passwords in this vault

d-vskscvt7 = Import preview
    .w-nothing-new-to-import = Nothing new to import
    .w-import-1-login = Import 1 login?
    .w-import-logins = Import { $len } logins?
    .w-close = Close
    .w-import = Import
    .w-cancel = Cancel
    .w-1-login-is-already-in-your = 1 login is already in your vault and will be skipped.
    .w-logins-are-already-in-your-vault = { $len } logins are already in your vault and will be skipped.
    .w-and-more = And { $value } more

d-qlke2mar = Export backup~present

d-wguekvub = Restore backup~present
