// SPDX-License-Identifier: AGPL-3.0-only
package org.asamk.signal.manager.config;

import java.net.URI;
import java.util.Optional;
import org.signal.network.config.HttpProxy;

// The signed launcher sanitizes the environment and supplies this local
// relay only after protected review. No host proxy or direct-network fallback.
final class TiloHttpProxy {
    static Optional<HttpProxy> nativeGate() {
        var value = System.getenv("HTTP_PROXY");
        if (value == null) { return Optional.empty(); }
        var uri = URI.create(value);
        if (!"http".equals(uri.getScheme()) || !"127.0.0.1".equals(uri.getHost())
                || uri.getPort() < 1 || uri.getPort() > 65535 || uri.getUserInfo() != null
                || uri.getQuery() != null || uri.getFragment() != null
                || !(uri.getPath().isEmpty() || uri.getPath().equals("/"))) {
            throw new IllegalArgumentException("Invalid owned network relay");
        }
        return Optional.of(new HttpProxy(uri.getHost(), uri.getPort()));
    }
}
