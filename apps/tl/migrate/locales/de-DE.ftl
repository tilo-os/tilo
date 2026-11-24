# Umzüge auf Deutsch. Keys are the declarations' stable ids (app.ids); the source's words are written from the package's sources.

d-so-tilo-migrate = Umzüge

d-so-tilo-migrate-keywords = umzug, umzüge, migrieren, importieren, übernehmen, thunderbird, browser, cloud-speicher, konten, ordner

d-fosjwamu = Eintrag
    .doc = Etwas, das die Durchsicht in der Quelle gefunden hat: ein Konto, ein Ordner, ein Adressbuch, ein Kalender.
    .noun = { $count ->
        [one] Eintrag
       *[other] Einträge
    }

d-z7gblqe6 = Schlüssel
    .doc = Was der Eintrag in der Quelle ist, wie die Engine ihn nennt, eindeutig im Umzug.

d-fvmu6lru = Art
    .doc = Was für ein Ding es ist: „Konto“, „Ordner“, „Adressbuch“.

d-ove3mkdj = Titel

d-7b7r6oxc = Anzahl
    .doc = Wie viele Dinge er enthält, gezählt, wie `counts` sagt.

d-kz76r36d = Gezählt
    .doc = Das Nomen im Plural für das, was `count` zählt: „Nachrichten“, „Kontakte“.

d-qubfwmfb = Unter
    .doc = Der Schlüssel des Eintrags, unter dem er steht; leer für einen ganz oben.

d-ghq4i3f7 = Ausgewählt
    .doc = Ob er übernommen werden soll.

d-2ogrf3gp = Hinweis
    .doc = Was die Durchsicht dazu sagt: „Mit dir geteilt, nur lesbar“.

d-fosjwamu-filter-count = Anzahl

d-fosjwamu-filter-chosen = Ausgewählt

d-7fp3ivdj = Anzahl
    .doc = Wie viele Dinge einer Art der letzte Lauf übernommen hat: „Kontakte“ 120.
    .noun = { $count ->
        [one] Anzahl
       *[other] Anzahlen
    }

d-3cttshux = Art

d-imoqqond = Anzahl

d-7fp3ivdj-filter-count = Anzahl

d-p7c5y2uc = Lauf
    .doc = Ein Lauf eines Umzugs und die Schlüssel dessen, was er übernommen hat, sodass sein Rückgängigmachen genau diese vergisst.
    .noun = { $count ->
        [one] Lauf
       *[other] Läufe
    }

d-wphp5gxk = Kennung
    .doc = Die Kennung des Laufs: der Aufruf von `start` der Engine, der ihn begonnen hat.

d-e73pil4f = Titel

d-ifezl4lz = Schlüssel

d-p7c5y2uc-filter-keys = Schlüssel

d-rj2svje3 = Umzug
    .doc = Was eine Quelle enthält, in den Arbeitsbereich holen: die persönlichen Ordner eines Computers, seine Browser, ein Thunderbird-Profil, ein E-Mail-Konto, einen Cloud-Speicher, die Seiten oder Vorgänge eines Dienstes.
    .noun = { $count ->
        [one] Umzug
       *[other] Umzüge
    }

d-jy4y5q35 = Quelle
    .doc = Der Schlüssel der Engine für die Quelle: home, browsers, thunderbird, mail, gdrive, onedrive, dropbox, icloud, nextcloud, webdav, outline, linear, forgejo.

d-d66kqgv4 = Titel
    .doc = Was die Quelle ist, wie ein Mensch es liest.

d-6muhvdtw = Gerät
    .doc = Der Schlüssel des Geräts, das die Daten hat und die Quelle ausführt; leer für eine Quelle, die jedes Gerät ausführen kann.

d-kxg2fplq = Profil
    .doc = Das Profil, Konto oder Benutzerverzeichnis, das die Durchsicht gelesen hat.

d-5nrd34wr = Einträge

d-dwktqelj = Nicht lesbar
    .doc = Was die Durchsicht nicht lesen konnte, jeweils in einem Satz.

d-cbtpcpsr = Stand

d-4tzslsk2 = Lauf
    .doc = Die Kennung des laufenden oder letzten Laufs.

d-c6gmooon = Titel des Laufs

d-v6notall = Fehler

d-6tvxvli7 = Abgelehnt
    .doc = Warum das Gerät den letzten vom Umzug verlangten Schritt abgelehnt hat; leer, sobald ein Schritt durchgeht.

d-wtqlrste = Übernommen
    .doc = Wie viele jeder Art der letzte Lauf übernommen hat.

d-hl5hwgex = Schon vorhanden
    .doc = Wie viele Dinge der letzte Lauf schon im Arbeitsbereich vorgefunden hat.

d-g44f5aep = Angekommen
    .doc = Die Schlüssel der Einträge aus dieser Quelle, die schon im Arbeitsbereich sind.

d-45kagnwx = Läufe
    .doc = Was jeder Lauf übernommen hat.

d-rj2svje3-filter-unreadable = Nicht lesbar

d-rj2svje3-filter-state = Stand

d-rj2svje3-filter-already_there = Schon vorhanden

d-rj2svje3-filter-landed = Angekommen

d-uqkltwh2 = Umzüge
    .doc = Die Umzüge des Arbeitsbereichs.
    .noun = { $count ->
        [one] Umzug
       *[other] Umzüge
    }

d-l7nahbny = Umzüge

d-dgwyov6w = Stand des Laufs
    .doc = Wo der Lauf eines Umzugs steht: durchgesehen, und was die Quelle enthält, wartet auf die Auswahl; läuft, während das Gerät mit den Daten das Ausgewählte übernimmt; von jemandem angehalten, wobei das bis dahin Übernommene bleibt; fehlgeschlagen, mit dem Grund; oder fertig.

d-wi42igkx = Durchgesehen

d-yc72xedi = Läuft

d-7lcij7kb = Angehalten

d-3ozfhjvz = Fehlgeschlagen

d-7pmvqxs4 = Fertig

d-znhlkiro = Durchgesehen
    .doc = Das Gerät, das `source` durchgesehen hat, führt sie auf: der Umzug für diese Quelle auf diesem Gerät, beim ersten Mal angelegt, mit seinem Titel und dem Profil, das die Durchsicht gelesen hat.
    .did = hat eine Quelle durchgesehen

d-znhlkiro-source = Quelle

d-znhlkiro-title = Titel

d-znhlkiro-device = Gerät

d-znhlkiro-handle = Profil

d-7mgub726 = Eintrag aus der Durchsicht
    .doc = Die Durchsicht hat den Eintrag `key` gefunden: er wird wie gefunden behalten, ein schon bekannter behält, ob jemand ihn ausgewählt hat; ein neuer ist ausgewählt, wie `chosen` sagt.

d-7mgub726-key = Schlüssel

d-7mgub726-kind = Art

d-7mgub726-title = Titel

d-7mgub726-count = Anzahl

d-7mgub726-counts = Gezählt

d-7mgub726-within = Unter

d-7mgub726-note = Hinweis

d-7mgub726-chosen = Ausgewählt

d-2e2qnaw6 = Einträge vergessen
    .doc = Eine neue Durchsicht hat die Einträge, deren Schlüssel nicht in `keep` stehen, nicht mehr gefunden: sie gehen.

d-2e2qnaw6-keep = Behalten

d-54ygmmcv = Nicht lesbar
    .doc = Die Durchsicht konnte `what` nicht lesen.

d-54ygmmcv-what = Was

d-vnfd46in = Übernehmen
    .doc = Wählt einen Eintrag zur Übernahme aus oder lässt ihn weg.

d-vnfd46in-i = I

d-vnfd46in-chosen = Ausgewählt

d-453xvewh = Schlüssel auswählen
    .doc = Wählt die Einträge mit den Schlüsseln `keys` aus oder lässt sie weg; mit `only` wird jeder andere Eintrag weggelassen.

d-453xvewh-keys = Schlüssel

d-453xvewh-chosen = Ausgewählt

d-453xvewh-only = Nur

d-6owc7q4m = Begonnen
    .doc = Das Gerät hat den Lauf `run` begonnen: er läuft, und was der letzte Lauf übernommen hat, wird geleert.

d-6owc7q4m-run = Lauf

d-6owc7q4m-title = Titel

d-sm5273v2 = Angekommen
    .doc = Der Lauf `run` hat die Einträge `keys` übernommen: `count` Dinge der Art `kind`, und `already` davon schon im Arbeitsbereich vorgefunden.

d-sm5273v2-run = Lauf

d-sm5273v2-keys = Schlüssel

d-sm5273v2-kind = Art

d-sm5273v2-count = Anzahl

d-sm5273v2-already = Schon da

d-mzjsihxi = Beendet
    .doc = Der Lauf endete als `state`: angehalten, fehlgeschlagen mit `failure`, oder fertig.
    .w-a-run-ends-stopped-failed-or = Ein Lauf endet angehalten, fehlgeschlagen oder fertig

d-mzjsihxi-state = Stand

d-mzjsihxi-failure = Fehler

d-skk653vz = Lauf vergessen
    .doc = Das Gerät hat aus dem Arbeitsbereich genommen, was der Lauf `run` übernommen hat: seine Schlüssel sind nicht mehr angekommen, und der Lauf ist fort.

d-skk653vz-run = Lauf

d-suc576de = Abgelehnt
    .doc = Das Gerät hat einen Schritt von `g` abgelehnt, mit `why` als Grund: nichts zum Übernehmen ausgewählt, eine Quelle, die es nicht lesen kann, oder ein Lauf, den es nicht zurücknehmen kann.

d-suc576de-why = Warum

d-mc6o2yv4 = Erneut nachsehen
    .doc = Liest die Quelle von `g` erneut: was sie jetzt enthält, steht zur Auswahl.
    .did = hat eine Quelle erneut durchgesehen

d-z7l36mcv = Starten
    .doc = Übernimmt das Ausgewählte aus der Quelle von `g`: das Gerät mit den Daten führt es als einen Lauf aus, und der Umzug folgt ihm dort, mit `begun`, `landed` und `ended`. Die Aktion dauert so lange wie der Lauf, und sie anzuhalten hält den Lauf an.
    .did = hat die Übernahme begonnen

d-b6a4nblx = Anhalten
    .doc = Hält den Lauf von `g` nach dem Eintrag an, an dem er gerade ist: was er übernommen hat, bleibt, und ein erneuter Start setzt ihn fort.
    .did = hat die Übernahme angehalten

d-x6sqvfbn = Rückgängig machen
    .doc = Nimmt aus dem Arbeitsbereich wieder heraus, was der Lauf `r` übernommen hat.
    .did = hat einen Lauf zurückgenommen

d-x6sqvfbn-r = R

d-rzpx6bzh = Beenden

d-vzbtgrfk = Umzüge
    .w-nothing-is-being-brought-in = Es wird nichts übernommen

d-gd45slyw = Umzugszeile
    .w-brought-in = { $brought } übernommen

d-m6xdvzmk = Umzugsseite
    .w-could-not-be-read = Nicht lesbar
    .w-what-to-bring-in = Was übernommen wird
    .w-the-review-found-nothing-to-bring = Die Durchsicht hat nichts zum Übernehmen gefunden
    .w-runs = Läufe

d-zwcqzi7i = Stand des Laufs
    .w-bringing-in = Wird übernommen
    .w-stop = Anhalten
    .w-resume = Fortsetzen
    .w-try-again = Erneut versuchen
    .w-bring-in = Übernehmen
    .w-look-again = Erneut nachsehen
    .w-the-run-stopped-on-an-error = Der Lauf ist mit einem Fehler stehen geblieben
    .w-that-could-not-be-done = Das ließ sich nicht tun
    .w-already-here = { $already_there } schon hier

d-toxseay6 = Eintragszeile

d-htm3bmqz = Laufzeile
    .w-items = { $len } Einträge
    .w-undo = Rückgängig
