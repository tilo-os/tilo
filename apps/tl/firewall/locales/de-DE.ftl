# tilolang messages: generated from the package's source, commit it. Translate in
# locales/<language>.ftl beside it; edit the words in the source, not here.

d-so-tilo-firewall = Firewall

d-so-tilo-firewall-keywords = firewall, firewalld, netzwerk, regeln, sicherheit, ports, zonen

d-scb6s6ci = Regel
    .doc = Eine Regel einer Zone: ein Dienst oder ein Port, was sie mit passendem Datenverkehr tut und woher er kommt.
    .noun = { $count ->
        [one] Regel
       *[other] Regeln
    }

d-eeqhnuyu = Titel
    .doc = Wie die Regel heißt: der Name eines Dienstes oder ihr Port.

d-3jndsc7i = Dienst
    .doc = Der Dienst, den sie trifft (`https`), oder leer für eine Port-Regel.

d-dlj2kbdr = Port
    .doc = Der Port oder Portbereich, den sie trifft (`8080`, `8000-8100`), oder leer für eine Dienst-Regel.

d-nconpw3q = Protokoll
    .doc = `tcp` oder `udp`.

d-2ov2rg7l = Urteil
    .doc = `allow` oder `deny`.

d-adadcwgr = Quelle
    .doc = Die Quelladresse oder das Netz; leer für überall.

d-td35s7rd = Aktiviert
    .doc = Ob firewalld sie auf jedem Gerät anwendet.

d-scb6s6ci-filter-enabled = Aktiviert

d-3jxrri5o = Zone
    .doc = Eine firewalld-Zone: was mit nicht passendem Datenverkehr geschieht, welche Schnittstellen und Dienste sie zulässt und ihre Regeln.
    .noun = { $count ->
        [one] Zone
       *[other] Zonen
    }

d-kzbzenxe = Art
    .doc = `drop`, `block`, `public`, `external`, `dmz`, `work`, `home`, `internal` oder `trusted`.

d-xthxweeb = Ziel
    .doc = Was mit Datenverkehr geschieht, den keine Regel traf: `default`, `accept`, `reject` oder `drop`.

d-6oz67tou = Schnittstellen
    .doc = Die Schnittstellen, die sie beansprucht.

d-fcur4ope = Dienste
    .doc = Die Dienste, die sie durchlässt.

d-vyfun5ze = Regeln
    .doc = Ihre Regeln, in der Reihenfolge, in der sie gelten.

d-bqtwzcsu = Maskierung
    .doc = Ob sie ausgehenden Datenverkehr maskiert.

d-3jxrri5o-filter-services = Dienste

d-3jxrri5o-filter-masquerade = Maskierung

d-a3e4xeyn = Firewall
    .doc = Die Firewall-Konfiguration des Arbeitsbereichs: ein Satz Zonen und Regeln, den jedes Gerät auf seine eigene firewalld anwendet.
    .noun = { $count ->
        [one] Firewall
       *[other] Firewalls
    }

d-fzjbpclp = Aktiviert
    .doc = Ob firewalld läuft.

d-lsn43itt = Panikmodus
    .doc = Panikmodus: der gesamte Datenverkehr ein- und ausgehend wird verworfen.

d-qzperbb7 = Aktive Zone
    .doc = Die aktive Zone.

d-fjnbtnf2 = Zonen
    .doc = Die konfigurierten Zonen.

d-a3e4xeyn-filter-enabled = Aktiviert

d-a3e4xeyn-filter-panic_mode = Panikmodus

d-z4c7ibjw = Anzuwendende Konfiguration
    .doc = Eine Konfiguration, wie das `apply`-Verb des Geräts sie liest: dieselben Felder, die die Messung trägt, damit eine Form hinaus- und zurückgeht.
    .noun = { $count ->
        [one] anzuwendende Konfiguration
       *[other] anzuwendende Konfigurationen
    }

d-z4rwsmkt = Aktiviert

d-hfx4peji = Panikmodus

d-wpzvhcny = Aktive Zone

d-fa3zy4tg = Zonen

d-z4c7ibjw-filter-enabled = Aktiviert

d-z4c7ibjw-filter-panic_mode = Panikmodus

d-gemauwoj = Anzuwendende Zone
    .noun = { $count ->
        [one] anzuwendende Zone
       *[other] anzuwendende Zonen
    }

d-ki3bym3t = Art

d-npflzozj = Ziel

d-yll5vuns = Schnittstellen

d-kasq6ajf = Dienste

d-rsyraw6p = Regeln

d-mi6vac6j = Maskierung

d-gemauwoj-filter-masquerade = Maskierung

d-iknj6ivk = Anzuwendende Regel
    .noun = { $count ->
        [one] anzuwendende Regel
       *[other] anzuwendende Regeln
    }

d-iddy2jy5 = Dienst

d-rukhcwu4 = Port

d-46qdyqd7 = Protokoll

d-b7tu4bhi = Urteil

d-v2uf2efw = Quelle

d-pqajeuq5 = Aktiviert

d-iknj6ivk-filter-enabled = Aktiviert

d-5iss4ixn = Dienstdefinition
    .doc = Ein bekannter Dienst, den firewalld kennt, angeboten im Dienste-Reiter und im Regelformular.
    .noun = { $count ->
        [one] Dienstdefinition
       *[other] Dienstdefinitionen
    }

d-splkq5cx = Schlüssel

d-um6wchuo = Name

d-3lp2tu4x = Ports

d-lr6vcvpw = Protokoll

d-dguxp5s2 = Beschreibung

d-rmbzih5s = Die firewalld dieses Geräts lesen
    .doc = Übernimmt, was die firewalld dieses Geräts hält: die Zonen, ihre Dienste und Regeln und die Schalter.
    .w-connected-to-firewalld = Mit firewalld verbunden

d-wvp77mci = Firewall starten
    .doc = Startet firewalld.
    .w-there-is-no-firewall-configuration-yet = Es gibt noch keine Firewall-Konfiguration

d-2jbc472l = Firewall stoppen
    .doc = Stoppt firewalld.

d-7fjzxy2n = Panikmodus einschalten
    .doc = Wechselt in den Panikmodus: der gesamte Datenverkehr ein- und ausgehend wird verworfen.
    .w-there-is-no-firewall-configuration-yet = Es gibt noch keine Firewall-Konfiguration

d-t5pt7a72 = Panikmodus ausschalten
    .doc = Verlässt den Panikmodus.

d-fas2qr4c = Zur aktiven Zone machen
    .doc = Macht eine Zone zur aktiven.
    .w-made-the-active-zone = { $zone_title } zur aktiven Zone gemacht

d-fas2qr4c-zone = Zone

d-t5sxzg6g = Eine Regel ein- oder ausschalten
    .doc = Schaltet eine Regel in einer Zone ein oder aus.

d-t5sxzg6g-zone = Zone

d-t5sxzg6g-rule = Regel

d-t5sxzg6g-on = An

d-3hwynxa2 = Einen Dienst erlauben
    .doc = Erlaubt einen Dienst in einer Zone oder erlaubt ihn nicht mehr.

d-3hwynxa2-zone = Zone

d-3hwynxa2-service = Dienst

d-3hwynxa2-on = An

d-ipsf2opk = Regel hinzufügen
    .doc = Fügt einer Zone eine Regel hinzu oder speichert die bearbeitete.
    .w-choose-a-service = Wähle einen Dienst
    .w-that-is-not-a-port-or = Das ist kein Port und kein Portbereich, den diese Firewall annimmt
    .w-that-is-not-an-ipv4-address = Das ist keine IPv4-Adresse und kein Netz
    .w-updated-the-rule-for = Die Regel für { $title } aktualisiert
    .w-added-a-rule-for = Eine Regel für { $title } hinzugefügt

d-ipsf2opk-zone = Zone

d-ipsf2opk-editing = Bearbeitung

d-ipsf2opk-as_service = Als Dienst

d-ipsf2opk-service = Dienst

d-ipsf2opk-port = Port

d-ipsf2opk-proto = Protokoll

d-ipsf2opk-verdict = Urteil

d-ipsf2opk-source = Quelle

d-qf5szroz = Regel löschen
    .doc = Löscht eine Regel aus einer Zone.
    .w-deleted-a-rule-from-the-zone = Eine Regel aus der Zone { $zone_title } gelöscht

d-qf5szroz-zone = Zone

d-qf5szroz-rule = Regel

d-bnokiyga = Verworfenen Datenverkehr zeigen
    .doc = Liest, was der Kernel in der letzten Stunde verworfen hat.

d-jrxpvoqv = Ankommen
    .doc = Beim Öffnen der App wird die firewalld dieses Geräts in die Konfiguration des Arbeitsbereichs gelesen, wenn diese noch keine hat.

d-xcdaj2hg = Regeltitel
    .w-port = Port { $port }

d-sydm4ev3 = Zonenname
    .w-drop = Verwerfen
    .w-block = Blockieren
    .w-public = Öffentlich
    .w-external = Extern
    .w-dmz = DMZ
    .w-work = Arbeit
    .w-home = Zuhause
    .w-internal = Intern
    .w-trusted = Vertrauenswürdig

d-o7s22iek = Zonenbeschreibung
    .w-incoming-traffic-is-dropped-without-a = Eingehender Datenverkehr wird ohne Antwort verworfen. Ausgehender Datenverkehr ist erlaubt.
    .w-incoming-connections-are-refused-outgoing-traffic = Eingehende Verbindungen werden abgelehnt. Ausgehender Datenverkehr ist erlaubt.
    .w-for-untrusted-public-networks-only-the = Für nicht vertrauenswürdige öffentliche Netze. Nur die Dienste, die du erlaubst, sind erreichbar.
    .w-for-the-outside-of-a-router = Für die Außenseite eines Routers, der dein Netz maskiert.
    .w-for-public-facing-hosts-with-limited = Für öffentlich erreichbare Rechner mit begrenztem Zugang zum internen Netz.
    .w-for-work-networks-where-you-mostly = Für Arbeitsnetze, in denen du den anderen Rechnern meist vertraust.
    .w-for-home-networks-where-you-mostly = Für Heimnetze, in denen du den anderen Rechnern meist vertraust.
    .w-for-internal-networks-where-you-mostly = Für interne Netze, in denen du den anderen Rechnern meist vertraust.
    .w-all-network-connections-are-accepted = Alle Netzwerkverbindungen werden angenommen.

d-hmkq2bbk = Zielworte
    .w-accepted = Angenommen
    .w-rejected = Abgelehnt
    .w-dropped = Verworfen
    .w-rejected-by-firewalld-s-default-policy = Abgelehnt durch die Standardrichtlinie von firewalld

d-bpd4g2ib = Portfehler
    .w-enter-a-port-or-a-port = Gib einen Port oder einen Portbereich ein.
    .w-use-a-port-or-a-range = Verwende einen Port oder einen Bereich wie 8000-8100.
    .w-use-a-port-from-1-to = Verwende einen Port von 1 bis 65535.
    .w-start-a-port-range-with-the = Beginne einen Portbereich mit dem niedrigeren Port.

d-d5qxe6az = Quellfehler
    .w-use-a-network-prefix-from-0 = Verwende eine Netzpräfix von /0 bis /32.
    .w-enter-an-ipv4-address-or-network = Gib eine IPv4-Adresse oder ein Netz ein, etwa 192.168.1.0/24.

d-nxrvlfzu = Verlaufsmaßstab
    .w-per-min = { $top } per { $minutes } min

d-b3fbncak = Titel des Blockierten
    .w-reject = Ablehnen
    .w-drop = Verwerfen
    .w-port = { $said } { $proto } port { $port }
    .w-traffic = { $said } { $proto } traffic

d-fudc3znt = Quellenangabe
    .w-from-1-address = von 1 Adresse
    .w-from-addresses = von { $sources } Adressen

d-7fs3cjxx = Starten~present

d-4r3hrblj = Panik einschalten~present

d-vxibtymz = Regel hinzufügen~present

d-okktoj56 = Fenster
    .w-enable-the-rule = Die Regel { $name } einschalten
    .w-firewall = Firewall
    .w-zones = Zonen
    .w-active = Aktiv
    .w-panic-mode-is-on = Der Panikmodus ist an
    .w-all-incoming-and-outgoing-network-traffic = Der gesamte ein- und ausgehende Netzwerkverkehr wird verworfen.
    .w-the-firewall-is-stopped = Die Firewall ist gestoppt
    .w-no-rules-are-enforced-until-you = Es werden keine Regeln durchgesetzt, bis du die Firewall einschaltest.
    .w-stop = Stoppen
    .w-start = Starten
    .w-leave-panic-mode = Panikmodus verlassen
    .w-panic-mode = Panikmodus
    .w-apply = Anwenden
    .w-apply-the-rules-to-the-system = Die Regeln auf das System anwenden
    .w-add-rule = Regel hinzufügen
    .w-read-firewalld = firewalld lesen
    .w-read-this-device-s-firewalld-again = Die firewalld dieses Geräts erneut lesen
    .w-make-active-zone = Zur aktiven Zone machen
    .w-allowed-services = Erlaubte Dienste
    .w-allow-rules = Erlaubende Regeln
    .w-deny-rules = Ablehnende Regeln
    .w-details = Details
    .w-interfaces = Schnittstellen
    .w-none = Keine
    .w-unmatched-traffic = Nicht passender Datenverkehr
    .w-masquerading = Maskierung
    .w-on = An
    .w-off = Aus
    .w-rules = Regeln
    .w-this-zone-has-no-rules-yet = Diese Zone hat noch keine Regeln.
    .w-sort-by = Sortieren nach
    .w-rule = Regel
    .w-descending = Absteigend
    .w-allow = Erlauben
    .w-deny = Ablehnen
    .w-edit = { $name } bearbeiten
    .w-delete = { $name } löschen
    .w-services = Dienste
    .w-dropped-traffic = Verworfener Datenverkehr
    .w-applying-the-rules-to-this-system = Die Regeln werden auf dieses System angewendet
    .w-adding-a-rule-to = Eine Regel zu { $zone_label } hinzufügen
    .w-add-a-rule-to = Eine Regel zu { $zone_label } hinzufügen
    .w-edit-the-rule-in = Die Regel in { $zone_label } bearbeiten
    .w-save-rule = Regel speichern
    .w-cancel = Abbrechen
    .w-match = Übereinstimmung
    .w-service = Dienst
    .w-port = Port
    .w-8080-or-8000-8100 = 8080 oder 8000-8100
    .w-protocol = Protokoll
    .w-tcp = TCP
    .w-udp = UDP
    .w-action = Aktion
    .w-source = Quelle
    .w-anywhere = Überall
    .w-an-ipv4-address-or-network-such = Eine IPv4-Adresse oder ein Netz, etwa 192.168.1.0/24
    .w-turn-off-the-firewall = Die Firewall ausschalten?
    .w-enter-panic-mode = Panikmodus einschalten?
    .w-turn-off-the-firewall-2 = Die Firewall ausschalten
    .w-drop-all-traffic = Den gesamten Datenverkehr verwerfen
    .w-no-rules-will-be-enforced-until = Bis du sie wieder einschaltest, werden keine Regeln durchgesetzt. Wenn dieser Rechner aus einem Netz erreichbar ist, das du nicht kontrollierst, kann diese Verbindung ihn ohne deine Regeln erreichen.
    .w-all-incoming-and-outgoing-traffic-is = Der gesamte ein- und ausgehende Datenverkehr wird verworfen, auch die Fernsitzung, mit der du diesen Rechner vielleicht gerade reparierst. Bereits verbundene Sitzungen werden gekappt.

d-vc4u4tnw = Nicht verfügbar
    .w-firewalld-isn-t-available = firewalld ist nicht verfügbar
    .w-this-computer-s-firewall-service-is = Der Firewall-Dienst dieses Rechners läuft nicht. Starte ihn und versuche es erneut.
    .w-try-again = Erneut versuchen

d-jydyulkn = Diensteseite
    .w-allow = { $name } erlauben
    .w-allowed = Erlaubt
    .w-blocked = Blockiert

d-qehgteth = Blockiert-Seite
    .w-dropped-traffic-couldn-t-be-read = Verworfener Datenverkehr konnte nicht gelesen werden
    .w-try-again = Erneut versuchen
    .w-read-what-the-kernel-dropped-in = Lies aus dem Journal dieses Geräts, was der Kernel in der letzten Stunde verworfen hat.
    .w-read-dropped-traffic = Verworfenen Datenverkehr lesen
    .w-reading-dropped-traffic-from-the-system = Verworfenen Datenverkehr wird aus dem Systemjournal gelesen…
    .w-packets-in-the-last-min = Pakete in den letzten { $blocked_window_minutes } Min.
    .w-refresh = Aktualisieren
    .w-nothing-was-dropped = Nichts wurde verworfen
    .w-the-kernel-logged-no-dropped-or = Der Kernel hat in den letzten { $blocked_window_minutes } Minuten keine verworfenen oder abgelehnten Pakete protokolliert.
    .w-a-full-bar-is = Ein voller Balken ist { $scale }

d-2p2n3yku = Gespeicherte Konfiguration anwenden
    .doc = Wendet die Konfiguration des Arbeitsbereichs auf die firewalld dieses Geräts an, wenn sie sie nicht führt: Eine Änderung, die ein anderes Gerät gemacht oder jemand zurückgenommen hat, erreicht auch die Firewall dieses Geräts.
