# Remote Desktop auf Deutsch. Keys sind die stabilen ids der Deklarationen (app.ids); die Wörter der Quelle stehen in den Quellen des Pakets.

d-so-tilo-remote = Entfernter Desktop

d-so-tilo-remote-keywords = remote desktop, vnc, ssh, screen sharing, remote access, remote control, remmina, krdc, remote desktop connection, mstsc, terminal, fernzugriff, fernwartung, remotedesktop, entfernter zugriff, bildschirmfreigabe, fernsteuerung, verbinden

d-zt74w6zn = Verbindung
    .doc = Eine gespeicherte Maschine: wo sie ist und wie du sie erreichst.
    .noun = { $count ->
        [one] Verbindung
       *[other] Verbindungen
    }
    .untitled = Unbenannte Verbindung

d-ssofhrgw = Name

d-dyz6c4fy = Host
    .doc = Der Hostname oder die Adresse, oder die genaue Endpunkt-ID eines Tilo-Geräts.

d-3zbula3z = Port

d-qudrxklp = Benutzer

d-auxqrphf = Art

d-e54cvtei = Anmeldung

d-jf7npid4 = Zuletzt verwendet
    .doc = Wann zuletzt eine Sitzung dorthin geöffnet wurde, von jedem Gerät aus.

d-poxhapg7 = Gruppe
    .doc = Ein Wort, unter dem die Verbindung abgelegt ist.

d-qjgd4xi7 = Favorit
    .doc = Oben in der Liste angeheftet.

d-zt74w6zn-filter-favorite = Favorit

d-zt74w6zn-filter-kind = Art

d-z57y6pux = Remote
    .doc = Die gespeicherten Verbindungen des Arbeitsbereichs.
    .noun = { $count ->
        [one] Remote
       *[other] Remotes
    }

d-yylg63a7 = Verbindungen

d-swfqwjuz = Sitzung
    .doc = Eine Sitzung, die dieses Gerät geöffnet hat: der Stream, der sie trägt, und wohin sie geht.
    .noun = { $count ->
        [one] Sitzung
       *[other] Sitzungen
    }
    .untitled = Sitzung

d-fpphznkb = Verbindung
    .doc = Die gespeicherte Verbindung, nach ihrer ID.

d-vvyt5lha = Name

d-amv62hzp = Art

d-l3jskskl = Job
    .doc = Der Stream, den das Gerät dafür ausführt, bis er geschlossen wird.

d-3cerrtf7 = Terminal
    .doc = Das Terminal, in dem eine SSH-Shell gezeigt wird.

d-gazx3juc = Störung
    .doc = Warum sie nie startete, in Worten, die das Fenster zeigt.

d-swfqwjuz-filter-kind = Art

d-qav4zrbn = Gesehen
    .doc = Was eine Maschine zuletzt tat, als dieses Gerät sie erreichte.
    .noun = { $count ->
        [one] gesehen
       *[other] gesehen
    }

d-rb2plwyy = Verbindung

d-hgy5dnvw = Online

d-qav4zrbn-filter-online = Online

d-5he4b25e = Geheimnis
    .doc = Ein Passwort oder die Passphrase eines Schlüssels, mit der sich dieses Gerät bei einer Verbindung anmeldet.
    .noun = { $count ->
        [one] Geheimnis
       *[other] Geheimnisse
    }

d-zfdaqp7c = Verbindung

d-fb6ane5u = Zweck
    .doc = `ssh-password`, `ssh-key-passphrase` oder `vnc-password`: womit es anmeldet.

d-d6i33xnv = Wert

d-m3ebnfat = Vertrauter Schlüssel
    .doc = Ein SSH-Schlüssel, dem dieses Gerät für eine Maschine vertraut, nachdem ihre Person ihn geprüft hat.
    .noun = { $count ->
        [one] vertrauter Schlüssel
       *[other] vertraute Schlüssel
    }

d-rgmdf7xq = Host

d-cxui2vf5 = Port

d-bmf56c7k = Algorithmus

d-n3ikzj6y = Fingerabdruck

d-m3ebnfat-filter-port = Port

d-mezjldxk = Hier
    .doc = Was dieses Gerät von sich aus behält: die Sitzungen, die es öffnete, und was es von jeder Maschine sah.
    .noun = { $count ->
        [one] hier
       *[other] hier
    }

d-mqn5woua = Sitzungen

d-b32nsjwp = Gesehen

d-zdbfp7w6 = Geheimnisse

d-jbjh2xut = Vertraut

d-owzmvujj = Schlüssel-ID
    .doc = Ein SSH-Schlüssel, wie ihn eine Maschine zeigte.
    .noun = { $count ->
        [one] Schlüssel-ID
       *[other] Schlüssel-IDs
    }

d-x5gkgjv7 = Algorithmus

d-5vpzlqkt = Fingerabdruck

d-maqwxymo = Gesagt
    .doc = Was ein Stream über sich selbst sagt, so wie `engine.state` es liest.
    .noun = { $count ->
        [one] gesagt
       *[other] gesagt
    }

d-azv5q3lb = Phase
    .doc = `connecting`, `waiting`, `connected`, `closed` oder `failed`.

d-app3bmk2 = Grund
    .doc = Warum es scheiterte, in den Worten der Engine.

d-5owfkzza = Algorithmus
    .doc = Der Schlüssel, den ein erster Kontakt zeigte.

d-unhnci5d = Fingerabdruck

d-gwkgueek = Erwartet
    .doc = Der Schlüssel, dem dieses Gerät vertraut, und der, den die Maschine stattdessen zeigte.

d-pqs4atu7 = Gezeigt

d-maqwxymo-filter-expected = Erwartet

d-maqwxymo-filter-observed = Gezeigt

d-w54efwpr = SSH-Anfrage
    .noun = { $count ->
        [one] SSH-Anfrage
       *[other] SSH-Anfragen
    }

d-qyt6kg5x = Host

d-hey2mljd = Port

d-lutwl6ee = Benutzer

d-iiovxv6t = Methode
    .doc = `password` oder `key`.

d-u55jtdje = Geheimnis

d-swuhzzts = Erwartet

d-i4deiyeq = Spalten

d-wrbw55me = Zeilen

d-w54efwpr-filter-port = Port

d-w54efwpr-filter-expected = Erwartet

d-w54efwpr-filter-columns = Spalten

d-w54efwpr-filter-rows = Zeilen

d-kyykz7ye = VNC-Anfrage
    .noun = { $count ->
        [one] VNC-Anfrage
       *[other] VNC-Anfragen
    }

d-jtksl5yz = Host

d-ypo5oexy = Port

d-q3gwteu3 = Passwort

d-kyykz7ye-filter-port = Port

d-yalxbxrd = Gestartet
    .noun = { $count ->
        [one] gestartet
       *[other] gestartet
    }

d-hk6v4nxp = Job

d-k6qck3ko = Sitzung
    .doc = Das Terminal, in dem die Shell eines SSH-Streams gezeigt wird.

d-ifi4bjre = Sendeanfrage
    .noun = { $count ->
        [one] Sendeanfrage
       *[other] Sendeanfragen
    }

d-jlt5sla7 = Auftrag

d-5kuradb3 = Text

d-7fakrm5j = Übertragungsanfrage
    .noun = { $count ->
        [one] Übertragungsanfrage
       *[other] Übertragungsanfragen
    }

d-376cxznk = Auftrag

d-da2gioaf = Lokaler Pfad

d-33n5eruw = Entfernter Pfad

d-757jhqkq = Verbindungsart
    .doc = Was eine gespeicherte Verbindung spricht.

d-3kvrtkb7 = Secure Shell: ein Terminal auf der Maschine
    .doc = Secure Shell: ein Terminal auf der Maschine.

d-oxeu4mhs = Virtual Network Computing: der Bildschirm der Maschine, von hier gesteuert
    .doc = Virtual Network Computing: der Bildschirm der Maschine, von hier gesteuert.

d-3iijeiru = Remote Desktop Protocol: gespeichert und bearbeitet wie die anderen, aber Remote öffnet keine
    .doc = Remote Desktop Protocol: gespeichert und bearbeitet wie die anderen, aber Remote öffnet keine.

d-6bk2qevf = Ein anderes Tilo-Gerät: sein Bildschirm, geteilt, sobald sein Besitzer zustimmt
    .doc = Ein anderes Tilo-Gerät: sein Bildschirm, geteilt, sobald sein Besitzer zustimmt.

d-clndfhov = Anmeldeverfahren
    .doc = Wie sich eine Verbindung anmeldet.

d-6wx5iksm = Passwort

d-3gvwro53 = Öffentlicher Schlüssel

d-3ca6rn4z = Das Tilo-Gerätenetz kennt beide Geräte: du musst nichts eintippen
    .doc = Das Tilo-Gerätenetz kennt beide Geräte: du musst nichts eintippen.

d-glspcxbm = Verbindung hinzufügen
    .doc = Speichert eine Maschine, um sie über SSH, VNC oder als Tilo-Gerät zu erreichen.
    .did = hat eine Verbindung hinzugefügt
    .w-a-port-is-1-65535 = Ein Port ist 1–65535

d-glspcxbm-name = Name

d-glspcxbm-host = Host

d-glspcxbm-kind = Art

d-glspcxbm-port = Port

d-glspcxbm-user = Benutzer

d-glspcxbm-auth = Anmeldung

d-glspcxbm-group = Gruppe

d-4kw7istb = Verbindung ändern
    .doc = Ändert, was eine gespeicherte Verbindung ist: nur was abweicht, wird geschrieben, damit zwei Personen, die eine Verbindung bearbeiten, die Änderungen der jeweils anderen behalten.
    .did = hat eine Verbindung geändert
    .w-a-port-is-1-65535 = Ein Port ist 1–65535

d-4kw7istb-name = Name

d-4kw7istb-host = Host

d-4kw7istb-kind = Art

d-4kw7istb-port = Port

d-4kw7istb-user = Benutzer

d-4kw7istb-auth = Anmeldung

d-4kw7istb-group = Gruppe

d-m7zeywa6 = Anheften
    .doc = Heftet eine Verbindung an den Anfang der Liste oder löst sie wieder.

d-m7zeywa6-pinned = Angeheftet

d-llemdksm = Verbindung löschen
    .doc = Löscht eine gespeicherte Verbindung für alle, mit denen sie geteilt ist; die Maschine selbst bleibt unverändert.
    .did = hat eine Verbindung gelöscht

d-llemdksm-c = C

d-fpkhjqie = Das Passwort auf diesem Gerät behalten
    .doc = Behält das Passwort oder die Schlüssel-Passphrase, mit denen sich dieses Gerät bei `c` anmeldet; ein leerer Wert vergisst beides.

d-fpkhjqie-secret = Geheimnis

d-mmg5rfeo = Letzte Nutzung setzen
    .doc = Vermerkt, dass jetzt eine Sitzung zu `c` geöffnet wurde.

d-pzurwzcz = Verbinden
    .doc = Öffnet eine laufende Sitzung zu einer gespeicherten Verbindung: ein Terminal für SSH, den Bildschirm der Maschine für VNC und ein Tilo-Gerät.
    .did = hat sich mit einer Maschine verbunden

d-5idxcote = Trennen
    .doc = Beendet eine Sitzung; die gespeicherte Verbindung bleibt.

d-7do2xgwn = Trennen
    .doc = Beendet die Sitzung zu `c`, die dieses Gerät offen hat.

d-xsvgobr2 = Beobachten
    .doc = Vermerkt, was dieses Gerät von der Maschine sah, die eine Sitzung erreichte: erreichbar, sobald sie antwortete, nicht erreichbar, sobald sie es nicht tat.

d-2trjne3k = Text senden
    .doc = Tippt `text` in eine offene SSH-Sitzung, als wäre es an ihrem Terminal getippt worden.
    .w-no-session-to-this-machine-is = Zu dieser Maschine ist hier keine Sitzung offen
    .w-text-goes-to-an-ssh-terminal = Text geht an ein SSH-Terminal

d-2trjne3k-text = Text

d-xw23tl6j = Datei senden
    .doc = Sendet eine Datei von diesem Gerät an die Maschine, zu der eine SSH-Sitzung offen ist.
    .w-no-ssh-session-to-this-machine = Zu dieser Maschine ist hier keine SSH-Sitzung offen
    .w-sending = { $local_path } wird gesendet

d-xw23tl6j-local_path = Lokaler Pfad

d-xw23tl6j-remote_path = Entfernter Pfad

d-zj5zfclm = Datei abholen
    .doc = Holt eine Datei von der Maschine, zu der eine SSH-Sitzung offen ist, auf dieses Gerät.
    .w-no-ssh-session-to-this-machine = Zu dieser Maschine ist hier keine SSH-Sitzung offen
    .w-fetching = { $remote_path } wird abgeholt

d-zj5zfclm-remote_path = Entfernter Pfad

d-zj5zfclm-local_path = Lokaler Pfad

d-frtkxqhc = Verschobene Datei
    .doc = Sagt, wie der Weg einer Datei zu oder von einer Maschine endete.
    .w-the-file-reached-its-place = Die Datei ist an ihrem Platz angekommen

d-frtkxqhc-j = J

d-xwz3xee3 = Schlüssel vertrauen
    .doc = Vertraut dem SSH-Schlüssel, den eine Maschine beim ersten Kontakt zeigte, nur auf diesem Gerät; erneutes Verbinden meldet an.
    .did = hat einem Schlüssel einer Maschine vertraut
    .w-no-session-to-this-machine-is = Zu dieser Maschine ist hier keine Sitzung offen
    .w-this-machine-shows-no-new-key = Diese Maschine zeigt keinen neuen Schlüssel zum Vertrauen
    .w-the-machine-s-key-changed-before = Der Schlüssel der Maschine änderte sich, bevor er vertraut wurde. Prüfe ihn erneut.
    .w-this-device-trusts-another-key-for = Dieses Gerät vertraut einem anderen Schlüssel für diese Maschine. Vergiss ihn zuerst.

d-xwz3xee3-fingerprint = Fingerabdruck

d-ze4shwms = Schlüssel vergessen
    .doc = Vergisst den Schlüssel, den dieses Gerät einer Maschine vertraut hat, deren Schlüssel sich änderte; erneutes Verbinden zeigt den neuen zum Prüfen.
    .w-this-device-trusts-no-key-for = Dieses Gerät vertraut keinem Schlüssel für diese Maschine
    .w-this-device-no-longer-trusts-the = Dieses Gerät vertraut dem alten Schlüssel nicht mehr

d-4flbj2vt = Formular speichern
    .doc = Speichert das Formular: fügt die Verbindung hinzu oder ändert die, über die es geöffnet wurde, und behält das Passwort auf diesem Gerät.

d-4flbj2vt-editing = Bearbeiten

d-4flbj2vt-name = Name

d-4flbj2vt-host = Host

d-4flbj2vt-port = Port

d-4flbj2vt-user = Benutzer

d-4flbj2vt-kind = Art

d-4flbj2vt-auth = Anmeldung

d-4flbj2vt-group = Gruppe

d-4flbj2vt-secret = Geheimnis

d-2hsyunxv = Anmeldewort
    .w-password = Passwort
    .w-public-key = Öffentlicher Schlüssel
    .w-device-identity = Geräteidentität

d-bj7owich = Namensproblem
    .w-a-connection-needs-a-name = Eine Verbindung braucht einen Namen

d-txir7jwg = Hostproblem
    .w-a-connection-needs-an-address = Eine Verbindung braucht eine Adresse
    .w-a-tilo-device-is-named-by = Ein Tilo-Gerät wird über seine genaue Endpunkt-ID benannt

d-fdzed5uz = Portproblem
    .w-a-port-is-1-65535 = Ein Port ist 1–65535

d-52xljp76 = Anmeldeproblem
    .w-ssh-signs-in-with-a-password = SSH meldet sich mit einem Passwort oder einem Schlüssel an
    .w-vnc-signs-in-with-a-password = VNC meldet sich mit einem Passwort an
    .w-rdp-signs-in-with-a-password = RDP meldet sich mit einem Passwort an
    .w-a-tilo-device-signs-in-with = Ein Tilo-Gerät meldet sich mit der Identität des Geräts an

d-fhnzw225 = Zeileninfo
    .w-rdp-isn-t-supported = RDP wird nicht unterstützt
    .w-needs-editing = Muss bearbeitet werden

d-r7g3wi7r = Nicht erreichbar
    .w-remote-opens-ssh-vnc-and-tilo = Remote öffnet SSH, VNC und Tilo-Geräte, aber kein RDP. Wähle SSH oder VNC, um diese Maschine weiter zu nutzen.
    .w-this-workspace-can-t-show-another = Dieser Arbeitsbereich kann den Bildschirm eines anderen Tilo-Geräts noch nicht zeigen.

d-6xxrt65p = Fenstertitel
    .w-new-connection = Neue Verbindung
    .w-edit-connection = Verbindung bearbeiten
    .w-session = Sitzung
    .w-pinned-connections = Angeheftete Verbindungen
    .w-connections = Verbindungen

d-vsdee2oi = Verbindung hinzufügen~present

d-q53vzy66 = Verbindung ändern~present

d-buhsdbzf = Anheften~present

d-3zyuqktu = Verbinden~present

d-hhjxomdo = Text senden~present

d-6hscyyuv = Datei ablegen~present

d-bzru2jt4 = Datei abholen~present

d-thmurafr = Host-Schlüssel vertrauen~present

d-tqrxlxsy = Host-Schlüssel vergessen~present

d-bccnijkx = Fenster
    .w-all = Alle
    .w-pinned = Angeheftet
    .w-session = Sitzung
    .w-save = Speichern
    .w-fit-to-window = An Fenster anpassen
    .w-actual-size = Originalgröße
    .w-full-screen = Vollbild
    .w-disconnect = Trennen
    .w-new-connection = Neue Verbindung
    .w-machine = Maschine
    .w-protocol = Protokoll
    .w-ssh = SSH
    .w-vnc = VNC
    .w-rdp = RDP
    .w-tilo-device = Tilo-Gerät
    .w-your-devices = Deine Geräte
    .w-online = Online
    .w-offline = Offline
    .w-name = Name
    .w-home-server = Heimserver
    .w-device = Gerät
    .w-device-endpoint-id = Endpunkt-ID des Geräts
    .w-host = Host
    .w-host-name-or-ip-address = Hostname oder IP-Adresse
    .w-port = Port
    .w-user = Benutzer
    .w-user-name = Benutzername
    .w-sign-in-with = Anmelden mit
    .w-key-passphrase = Schlüssel-Passphrase
    .w-password = Passwort
    .w-kept-on-this-device-only = Nur auf diesem Gerät gespeichert.
    .w-optional = Optional
    .w-group = Gruppe
    .w-delete-connection = Verbindung löschen
    .w-remote-terminal = Remote-Terminal, { $name }
    .w-remote-desktop = Remote-Desktop, { $name }
    .w-connecting-to = Verbindung zu { $name } wird aufgebaut
    .w-remote-is-reaching-the-machine-and = Remote erreicht die Maschine und meldet sich an. Es bricht nach 45 Sekunden von selbst ab.
    .w-stop = Stopp
    .w-session-ended = Sitzung beendet
    .w-closed-the-session = { $name } hat die Sitzung geschlossen.
    .w-reconnect = Neu verbinden
    .w-couldn-t-connect-to = Verbindung zu { $name } nicht möglich
    .w-trust-key = Schlüssel vertrauen
    .w-forget-key = Schlüssel vergessen
    .w-remote-can-t-open-rdp-connections = Remote kann keine RDP-Verbindungen öffnen
    .w-close = Schließen
    .w-try-again = Erneut versuchen
    .w-not-connected = Nicht verbunden
    .w-no-session-to-is-open = Zu { $name } ist keine Sitzung offen.
    .w-connect = Verbinden
    .w-this-connection-was-deleted = Diese Verbindung wurde gelöscht
    .w-someone-you-share-your-connections-with = Jemand, mit dem du deine Verbindungen teilst, hat sie entfernt.
    .w-no-saved-connections = Keine gespeicherten Verbindungen
    .w-save-a-machine-to-reach-it = Speichere eine Maschine, um sie über SSH, VNC oder als Tilo-Gerät zu erreichen.
    .w-no-pinned-connections = Keine angehefteten Verbindungen
    .w-pin-the-machines-you-use-most = Hefte die Maschinen an, die du am meisten nutzt, damit sie hier bleiben.
    .w-no-connections-match = Keine passenden Verbindungen
    .w-edit-connection = Verbindung bearbeiten
    .w-other-connections = Andere Verbindungen
    .w-connect-to = Mit { $name } verbinden?
    .w-cancel = Abbrechen
    .w-this-opens-a-live-session-to = Dies öffnet eine laufende Sitzung zu { $address }. Remote prüft die gespeicherte Adresse und den Zugriff erneut, bevor etwas gesendet wird.
    .w-close-the-session-to = Die Sitzung zu { $name } schließen?
    .w-close-session = Sitzung schließen
    .w-the-live-session-ends-and-its = Die laufende Sitzung endet und ihr Verlauf geht mit. Die gespeicherte Verbindung bleibt.
    .w-trust-the-ssh-key-of = Dem SSH-Schlüssel von { $name } vertrauen?
    .w-check-this-key-with-the-machine = Prüfe diesen Schlüssel mit dem Besitzer der Maschine, bevor du ihm vertraust. Nur dieses Gerät merkt sich ihn.

d-sx7ujhdz = Neue Verbindung
