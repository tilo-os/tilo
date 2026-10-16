// SPDX-License-Identifier: MIT
package android.app;

import android.os.Bundle;
import android.widget.TextView;

/** Visible runtime surface for recovered foreground services, never app Activity callbacks. */
public final class TiloRecoveryActivity extends Activity {
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        TextView identity = new TextView(this);
        identity.setText(getPackageName());
        setContentView(identity);
        get_atl_loaded_app().tiloRecoverForegroundServices();
    }
}
