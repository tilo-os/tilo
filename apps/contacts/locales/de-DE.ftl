contacts-accounts = Kontoverknüpfungen
contacts-accounts-unavailable = Kontoverknüpfungen sind in dieser Sitzung nicht verfügbar.
contacts-account-person = Kontakt
contacts-account-service = Dienst
contacts-account-recipient = Adresse oder Kennung des Kontos
contacts-account-add = Verknüpfung prüfen
contacts-account-remove = Verknüpfung entfernen
contacts-account-review = Kontoverknüpfung prüfen
contacts-account-cancel = Abbrechen
contacts-account-confirm = Konto verknüpfen
contacts-account-invalid = Gib eine gültige Kennung des Kontos für den gewählten Dienst ein.
contacts-account-stale = Dieser Kontakt oder dieses Konto hat sich geändert. Öffne den Konto-Editor und prüfe erneut.
contacts-account-conflict = Dieses Konto ist bereits verknüpft. Prüfe die alte Verknüpfung und entferne sie zuerst.
contacts-account-unverified = Identitätsreferenz nicht verfügbar
contacts-account-linked = Verknüpft
contacts-account-conflicted = Zu prüfen
contacts-account-hint = Füge diesem Kontakt eine Adresse hinzu. Eine übereinstimmende Telefonnummer oder ein Name bestätigt keine Identität.
contacts-account-confirm-hint = Bestätige, dass dieses Konto zum gewählten Kontakt gehört. Das prüft keinen Besitz, teilt keine Dateien und vergibt keine Berechtigungen.
contacts-account-revoke-hint = Diese Verknüpfung aus der künftigen Zustellung entfernen. Bestehende Zuordnungen von Nachrichten bleiben unverändert.
contacts-account-reference = Identitätsreferenz
contacts-account-old-reference = Frühere Identitätsreferenz
contacts-account-edit = Verknüpfung bearbeiten
contacts-account-edited-unverified = Eine Änderung dieser Adresse löscht ihre alte Identitätsreferenz. Senden bleibt aus, bis die neue Identität des Kontos geprüft ist.

contacts-account-scope = Dienstkonto
contacts-account-review-description = { $action }: Kontakt { $person }. Dienst { $service }. Konto { $recipient }. Dienstkonto { $scope }. Identitätsreferenz { $revision }. Frühere Identitätsreferenz { $previous }.

## Verlauf: was jede Änderung am Adressbuch getan hat

contacts-new-contact = { $name } hinzugefügt
contacts-add-contacts = Zum Adressbuch hinzugefügt
contacts-delete-contact = { $item } gelöscht
contacts-merge-contacts = Einen Doppeleintrag in { $item } zusammengeführt
contacts-contact-first = Den Vornamen von { $item } geändert
contacts-contact-last = Den Nachnamen von { $item } geändert
contacts-contact-org = Die Organisation von { $item } geändert
contacts-contact-title = Den Titel von { $item } geändert
contacts-contact-website = Die Website von { $item } geändert
contacts-contact-birthday = Den Geburtstag von { $item } geändert
contacts-contact-notes = Die Notizen zu { $item } geändert
contacts-contact-favorite = { $new ->
    [true] { $item } zu den Favoriten hinzugefügt
   *[false] { $item } aus den Favoriten entfernt
}
contacts-contact-phones = { $op ->
    [insert] Eine Telefonnummer zu { $item } hinzugefügt
    [remove] Eine Telefonnummer von { $item } entfernt
   *[other] Die Telefonnummern von { $item } umsortiert
}
contacts-contact-emails = { $op ->
    [insert] Eine E-Mail-Adresse zu { $item } hinzugefügt
    [remove] Eine E-Mail-Adresse von { $item } entfernt
   *[other] Die E-Mail-Adressen von { $item } umsortiert
}
contacts-contact-addresses = { $op ->
    [insert] Eine Postadresse zu { $item } hinzugefügt
    [remove] Eine Postadresse von { $item } entfernt
   *[other] Die Postadressen von { $item } umsortiert
}
contacts-contact-groups = { $op ->
    [insert] { $item } zu einer Gruppe hinzugefügt
    [remove] { $item } aus einer Gruppe entfernt
   *[other] Die Gruppen von { $item } umsortiert
}
contacts-labeled-label = Den Typ von { $item } geändert
contacts-labeled-value = { $item } geändert
contacts-address-label = Den Typ einer Adresse geändert
contacts-address-street = Die Straße einer Adresse geändert
contacts-address-city = Die Stadt einer Adresse geändert
contacts-address-region = Die Region einer Adresse geändert
contacts-address-postal = Die Postleitzahl einer Adresse geändert
contacts-address-country = Das Land einer Adresse geändert
contacts-group-entry-value = Die Gruppe { $item } umbenannt

## Wie die App sich und ihre Liste nennt

contacts-title = Kontakte
