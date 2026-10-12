/* SPDX-License-Identifier: AGPL-3.0-only
 * Tilo's local account evidence extension for pinned signal-cli 0.14.8.
 * Loaded before the upstream jar. No registration, network lookup, trust
 * mutation or credential export occurs here: these are the owned managers.
 */
package org.asamk.signal.commands;

import net.sourceforge.argparse4j.inf.Namespace;
import net.sourceforge.argparse4j.inf.Subparser;
import org.asamk.signal.manager.MultiAccountManager;
import org.asamk.signal.output.JsonWriter;
import org.asamk.signal.output.OutputWriter;
import org.asamk.signal.output.PlainTextWriter;

public class ListAccountsCommand implements JsonRpcMultiLocalCommand {
    @Override
    public String getName() { return "listAccounts"; }

    @Override
    public void attachToSubparser(final Subparser subparser) {
        subparser.help("Show local registered account identities");
    }

    @Override
    public void handleCommand(final Namespace ns, final MultiAccountManager manager,
                              final OutputWriter output) {
        var rows = manager.getManagers().stream()
                .map(account -> new LocalAccount(account.getSelfNumber(), account.getSelfACI()))
                .toList();
        switch (output) {
            case JsonWriter json -> json.write(rows);
            case PlainTextWriter text -> rows.forEach(row -> text.println("Number: {}", row.number()));
        }
    }

    private record LocalAccount(String number, String aci) {}
}
