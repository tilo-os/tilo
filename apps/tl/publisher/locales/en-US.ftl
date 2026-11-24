# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-publisher = App Publisher

d-so-tilo-publisher-keywords = publish, release, store upload, app release, submit app, package, signing, deploy, distribute, developer, veröffentlichen, veroeffentlichen, bereitstellen, paketieren, signieren, entwickler, app einreichen

d-zzcx6gzr = Reading
    .doc = What the registry answered about the application, when it was asked: its state, the namespaces and any reason, with the moment of the check.
    .noun = { $count ->
        [one] reading
       *[other] readings
    }

d-hia3wxhi = At
    .doc = When the application was checked.

d-ntbsg676 = Summary
    .doc = The registry's own line: the state, the namespaces and any reason.

d-zzcx6gzr-filter-at = At

d-3n5i6fkg = Publisher
    .doc = The App Publisher: the key that signs your apps, and the namespaces the registry lets it publish under.
    .noun = { $count ->
        [one] publisher
       *[other] publishers
    }
    .untitled = App Publisher

d-izqbnlov = Registry
    .doc = The registry the key publishes to: credential-free https, or plain http on this machine.

d-4hwurbpa = Domain
    .doc = A domain whose namespace to claim, such as `example.com`.

d-6fcxou37 = Forge account
    .doc = A forge account whose namespace to claim, such as `github:ada`.

d-22elkpx4 = Contact
    .doc = How the registry's owner can reach the person about the application.

d-rgwkewz7 = Key
    .doc = This computer's key, as last read: `missing`, `present` or `unreadable`.

d-b63dald4 = Public key
    .doc = The key's public half, as hexadecimal; empty unless it is present.

d-b6h3el5f = Notice kind
    .doc = What the last operation said: its kind (`info`, `success` or `error`), its headline and the detail under it.

d-laxvdhhk = Notice title

d-ara6pea6 = Notice detail

d-vu53jaba = Job
    .doc = The registry call under way, by the job this computer gave it; empty when nothing runs.

d-etzmfdag = Readings
    .doc = What the application checks said, newest first.

d-viqgi64q = Forge parts
    .doc = A forge account as the registry reads one: the host and the account, its name lowercased.
    .noun = { $count ->
        [one] forge part
       *[other] forge parts
    }

d-y36hrf57 = Host

d-iwdc7ejj = Account

d-yte5dfvf = Key file
    .doc = The key file as text, for a copy or an export; never a field.
    .noun = { $count ->
        [one] key file
       *[other] key files
    }

d-6347lg4i = Text

d-lq7hftxp = Apply ask
    .doc = The namespaces the profile claims, as the engine reads them: one application for all of them.
    .noun = { $count ->
        [one] apply ask
       *[other] apply asks
    }

d-gh566qcn = Registry

d-eb4v6asr = Domain

d-hjj2vyhq = Forge account

d-e2wp4tw7 = Contact

d-addusg2k = Status ask
    .doc = The registry whose application is read.
    .noun = { $count ->
        [one] status ask
       *[other] status asks
    }

d-lbble236 = Registry

d-kdearqwg = Registry words
    .doc = The registry's own line about the application, once its call ended.
    .noun = { $count ->
        [one] registry word
       *[other] registry words
    }

d-2gablpyd = Summary

d-mo2pqvcq = Key state
    .doc = This computer's key, as the engine reads it.
    .noun = { $count ->
        [one] key state
       *[other] key states
    }

d-iyvbvvra = State

d-5gktvyxf = Public

d-tu4ielzu = Read key
    .doc = Reads this computer's publisher key: whether it has one, and its public half.

d-2wikw7t7 = Open key
    .doc = Opening the app reads this computer's key.

d-xjn27let = Create key
    .doc = Makes this computer's publisher key.
    .w-created-your-publisher-key = Created your publisher key
    .w-back-up-publisher-key-json-it = Back up publisher-key.json: it is the only copy. Public key { $public }.

d-7rwt45qt = Copy proof
    .doc = Copies the proof line: the words to publish where only the person can write.
    .w-this-computer-has-no-publisher-key = This computer has no publisher key yet.
    .w-copied-the-proof-line = Copied the proof line.

d-cp6smsn5 = Copy secret
    .doc = Copies the key file, for the person's CI: one secret, `TILO_PUBLISHER_KEY`, holding it.
    .w-copied-the-key = Copied the key
    .w-paste-it-into-your-ci-s = Paste it into your CI’s TILO_PUBLISHER_KEY secret. The clipboard keeps it until something else is copied.

d-mje63lgo = Export key…
    .doc = The key file, for the person to keep somewhere safe: what they restore the key from.
    .w-exported-your-publisher-key = Exported your publisher key
    .w-keep-publisher-key-json-somewhere-safe = Keep publisher-key.json somewhere safe; it is the only copy outside this device.

d-uiuxe7rl = Apply
    .doc = Asks the registry for the namespaces the profile's domain and forge account name.
    .did = applied for namespaces
    .w-the-registry-has-not-answered-yet = The registry has not answered yet
    .w-a-registry-is-reached-over-credential = A registry is reached over credential-free https, or plain http on this machine alone

d-cyealnod = Check application
    .doc = Reads what the registry holds about this computer's key's application.
    .did = checked the application
    .w-the-registry-has-not-answered-yet = The registry has not answered yet
    .w-a-registry-is-reached-over-credential = A registry is reached over credential-free https, or plain http on this machine alone

d-yqdomzqj = Answered
    .doc = The registry answered: what it said about the application, or why it could not.
    .w-no-application-yet = No application yet
    .w-this-registry-has-no-application-from = This registry has no application from this key yet.
    .w-application-status = Application status
    .w-the-registry-call-was-cancelled = The registry call was cancelled.

d-yqdomzqj-j = J

d-as6a7tpg = Apply for namespaces~present

d-3p63wowc = Check status~present

d-32nhaxk2 = Publisher window
    .w-that-didn-t-work = That didn’t work

d-l2p3ionf = Key section
    .w-signing-key = Signing key
    .w-public-key = Public key
    .w-proof-line = Proof line
    .w-publish-this-line-where-only-you = Publish this line where only you can write, to prove a namespace.
    .w-copy-proof-line = Copy proof line
    .w-copy-key-for-ci = Copy key for CI
    .w-export-key = Export key…
    .w-check-application = Check application
    .w-this-computer-has-no-publisher-key = This computer has no publisher key yet. It signs your apps and your requests to the registry.
    .w-create-key = Create key
    .w-the-publisher-key-can-t-be = The publisher key can’t be read
    .w-publisher-key-json-is-not-a = publisher-key.json is not a valid publisher key. Restore it from a backup, or move it aside and create a new key.
    .w-reading-this-computer-s-key = Reading this computer’s key…

d-osoofaou = Proof places section
    .w-where-to-publish-the-proof = Where to publish the proof
    .w-name-a-domain-or-a-forge = Name a domain or a forge account to see where its proof goes.
    .w-put-the-proof-line-where-only = Put the proof line where only you can write it, then apply.
    .w-publish-at = Publish at

d-p2cd4gqi = Namespaces section
    .w-namespaces = Namespaces
    .w-claim-the-domains-and-forge-accounts = Claim the domains and forge accounts whose apps you publish.
    .w-domain = Domain
    .w-example-com = example.com
    .w-proved-with-a-dns-txt-record = Proved with a DNS TXT record or a .well-known file on the domain.
    .w-forge-account = Forge account
    .w-github-ada = github:ada
    .w-proved-with-a-tilo-publisher-repository = Proved with a tilo-publisher repository on that account.
    .w-contact = Contact
    .w-you-example-com = you@example.com
    .w-how-the-registry-s-owner-can = How the registry’s owner can reach you about the application.
    .w-registry = Registry
    .w-https-apps-tilo-so = https://apps.tilo.so
    .w-a-registry-is-reached-over-credential = A registry is reached over credential-free https, or plain http on this machine alone.

d-66hkefo7 = Apply row
    .w-apply = Apply
    .w-waiting-for-the-registry = Waiting for the registry…

d-kn4pythb = Status section
    .w-application-status = Application status
    .w-no-application-status-has-been-checked = No application status has been checked here yet.
