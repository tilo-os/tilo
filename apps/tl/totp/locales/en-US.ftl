# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-totp = Authenticator

d-so-tilo-totp-keywords = authenticator, two-factor, 2fa, totp, otp, one-time code, verification code, security key, authentifizierung, zwei-faktor, einmalcode, sicherheitsschlüssel

d-a4pizi6g = Account
    .doc = One account a service asks a one-time code for: its name and the code it makes now.
    .noun = { $count ->
        [one] account
       *[other] accounts
    }
    .untitled = Account

d-wn7r7rgq = Issuer
    .doc = The service it is for, which names the account.

d-k6loujfj = Label
    .doc = Who it signs in as: an email, a username.

d-plyagryo = Algorithm
    .doc = The hash the code is made with: `SHA1`, `SHA256` or `SHA512`.

d-yycfj25v = Digits

d-hjsiidku = Period
    .doc = The seconds a code lasts: 30 for nearly every service.

d-2f4aknif = Color
    .doc = A colour slot the card shows it by.

d-owpfvymk = Code
    .doc = The code as of `at`, grouped in threes.

d-zwq4t3wg = Next
    .doc = The instant the code changes, and the one it was made at, in milliseconds since 1970.

d-kwphnayw = Changed

d-skneduwj = Vault
    .doc = The workspace's Authenticator: the accounts its members keep, and what the window last said.
    .noun = { $count ->
        [one] vault
       *[other] vaults
    }

d-4rdnjfde = Accounts

d-tcfimqtj = Notice

d-glvalakf = Notice kind

d-kmyqik2v = Code ask
    .noun = { $count ->
        [one] code ask
       *[other] code asks
    }

d-mv6qivu3 = Account

d-va7bpptd = Algorithm

d-2i4rwubi = Digits

d-5chme642 = Period

d-djhsbhlj = At ms

d-flvplbfi = At

d-kmyqik2v-filter-digits = Digits

d-kmyqik2v-filter-period = Period

d-kmyqik2v-filter-at_ms = At ms

d-hyfnt6bs = Code answer
    .noun = { $count ->
        [one] code answer
       *[other] code answers
    }

d-xdlnshvl = Code

d-s4atxkzh = Left

d-hyfnt6bs-filter-left = Left

d-yv4c73ut = Parse ask
    .noun = { $count ->
        [one] parse ask
       *[other] parse asks
    }

d-qhgdgyyn = Uri

d-hch5m373 = Parsed answer
    .noun = { $count ->
        [one] parsed answer
       *[other] parsed answers
    }

d-x5elnfmo = Issuer

d-ac2s7eqm = Label

d-nsnnmxnd = Secret

d-234azfht = Algorithm

d-cikeo42j = Digits

d-p5k5ujrm = Period

d-hch5m373-filter-digits = Digits

d-hch5m373-filter-period = Period

d-ytz757ob = Scan ask
    .noun = { $count ->
        [one] scan ask
       *[other] scan asks
    }

d-j757xqu7 = Address

d-hqh5cg2d = Scan answer
    .noun = { $count ->
        [one] scan answer
       *[other] scan answers
    }

d-4jvzelry = Uri

d-ebyvkahl = Check ask
    .noun = { $count ->
        [one] check ask
       *[other] check asks
    }

d-shi3ofgj = Secret

d-a6bx5sr3 = Check answer
    .noun = { $count ->
        [one] check answer
       *[other] check answers
    }

d-pon234hn = Ok

d-a6bx5sr3-filter-ok = Ok

d-y6idfclf = Keep ask
    .noun = { $count ->
        [one] keep ask
       *[other] keep asks
    }

d-amwu66xm = Account

d-md2augxf = Secret

d-ea4q75q2 = Seed ask
    .noun = { $count ->
        [one] seed ask
       *[other] seed asks
    }

d-45a67ziv = Account

d-sy74ekqy = Seed answer
    .noun = { $count ->
        [one] seed answer
       *[other] seed answers
    }

d-zqveq2mg = Secret

d-r5hx7nb7 = Forget ask
    .noun = { $count ->
        [one] forget ask
       *[other] forget asks
    }

d-3pw6frem = Account

d-ec2dszgu = Add account
    .doc = Starts an account from what the person typed, or what a setup link said: what an agent, a script or the form asks.
    .did = added an account
    .w-the-account-needs-a-service-name = The account needs a service name
    .w-the-account-needs-the-key-the = The account needs the key the service gave
    .w-a-code-has-4-to-10 = A code has 4 to 10 digits
    .w-a-code-changes-every-5-to = A code changes every 5 to 300 seconds

d-ec2dszgu-issuer = Issuer

d-ec2dszgu-label = Label

d-ec2dszgu-secret = Secret

d-ec2dszgu-algorithm = Algorithm

d-ec2dszgu-digits = Digits

d-ec2dszgu-period = Period

d-ev44bsqe = Add from a setup link
    .doc = Adds the account a setup link names: what the QR scan and the paste field both end in.
    .did = added an account from a setup link

d-ev44bsqe-uri = Uri

d-54fksuzg = Add from a picture
    .doc = Adds the account a picture's QR code names.
    .did = added an account from a picture

d-54fksuzg-file = File

d-uzlj3vkz = Save the account
    .doc = Changes what an account says, its key included (a service that reissued it).
    .did = changed an account
    .w-the-account-needs-a-service-name = The account needs a service name
    .w-the-account-needs-the-key-the = The account needs the key the service gave
    .w-a-code-has-4-to-10 = A code has 4 to 10 digits
    .w-a-code-changes-every-5-to = A code changes every 5 to 300 seconds
    .w-the-key-could-not-be-kept = The key could not be kept on this device

d-uzlj3vkz-issuer = Issuer

d-uzlj3vkz-label = Label

d-uzlj3vkz-secret = Secret

d-uzlj3vkz-algorithm = Algorithm

d-uzlj3vkz-digits = Digits

d-uzlj3vkz-period = Period

d-2jxhhwxw = Move up
    .doc = Moves an account one place up the list.
    .did = moved an account up

d-2jxhhwxw-a = A

d-esml4kb5 = Move down
    .doc = Moves an account one place down the list.
    .did = moved an account down

d-esml4kb5-a = A

d-g3zipnp4 = Delete the account
    .doc = Deletes an account and the key this device kept for it.
    .did = deleted an account

d-g3zipnp4-a = A

d-bqscqz5d = Copy code
    .doc = Copies an account's current code to the clipboard.
    .did = copied a code

d-koebj2v6 = Refresh
    .doc = Makes every account's current code: what the window's clock trigger runs each second, and what the list reads.

d-y5xpyf3z = Refresh one

d-rhivupj2 = Refuse

d-rhivupj2-why = Why

d-jcvyrcwb = Save form
    .doc = Saves the form: starts the account, or changes the one it was opened over.

d-jcvyrcwb-editing = Editing

d-jcvyrcwb-issuer = Issuer

d-jcvyrcwb-label = Label

d-jcvyrcwb-secret = Secret

d-jcvyrcwb-algorithm = Algorithm

d-jcvyrcwb-digits = Digits

d-jcvyrcwb-period = Period

d-htudz4b7 = Clear notice

d-vmyv2aoh = Add account~present

d-3gvwp5ni = Update account~present

d-4dgdsgtn = Vault window
    .w-authenticator = Authenticator
    .w-add-account = Add account
    .w-back = Back
    .w-scan-a-qr-image = Scan a QR image
    .w-save = Save
    .w-edit-account = Edit account
    .w-scan-a-qr-code-paste-a = Scan a QR code, paste a setup link, or type the details the service gave.
    .w-setup-link = Setup link
    .w-add-from-the-link = Add from the link
    .w-service-name = Service name
    .w-email-or-username = Email or username
    .w-key-from-the-service = Key from the service
    .w-advanced-options = Advanced options
    .w-algorithm = Algorithm
    .w-sha1 = SHA1
    .w-sha256 = SHA256
    .w-sha512 = SHA512
    .w-digits = Digits
    .w-period-in-seconds = Period in seconds
    .w-dismiss = Dismiss
    .w-no-accounts-yet = No accounts yet
    .w-scan-a-qr-code-or-paste = Scan a QR code or paste a setup link to see its one-time codes here.
    .w-account = Account
    .w-code-lifetime = Code lifetime
    .w-copy = Copy
    .w-edit = Edit
    .w-more = More
    .w-move-up = Move up
    .w-move-down = Move down
    .w-delete-account = Delete account

d-cmp6cz3k = Status
