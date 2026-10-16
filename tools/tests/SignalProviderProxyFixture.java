// SPDX-License-Identifier: AGPL-3.0-only
package org.asamk.signal.manager.config;

import java.util.List;

public class SignalProviderProxyFixture {
    public static void main(String[] args) {
        try {
            var proxy = LiveConfig.createDefaultServiceConfiguration(List.of()).getSystemHttpProxy();
            if (args[0].equals("empty")) {
                if (proxy.isPresent()) { throw new AssertionError("Unreviewed proxy present"); }
            } else if (args[0].equals("local")) {
                var local = proxy.orElseThrow();
                if (!local.getHost().equals("127.0.0.1") || local.getPort() != 32123) {
                    throw new AssertionError("Owned relay differs");
                }
            } else { throw new AssertionError("Foreign relay accepted"); }
        } catch (ExceptionInInitializerError error) {
            if (!args[0].equals("invalid") || !(error.getCause() instanceof IllegalArgumentException)) {
                throw error;
            }
        }
        System.out.println("Pinned provider proxy fixture passed: " + args[0]);
    }
}
