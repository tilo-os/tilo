// SPDX-License-Identifier: MIT
package android.app;

/** Publishes Android content descriptions through GTK's existing AT-SPI tree. */
public final class TiloAccessibility {
    private TiloAccessibility() {}
    private static native void describeNative(long widget, String label, boolean textView);
    public static void describe(long widget, String label, boolean textView) {
        TiloChooser.loadBridge();
        describeNative(widget, label, textView);
    }
}
