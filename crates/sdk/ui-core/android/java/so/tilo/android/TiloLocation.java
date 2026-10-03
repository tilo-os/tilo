package so.tilo.android;

import android.content.Context;
import android.location.Location;
import android.location.LocationListener;
import android.location.LocationManager;
import android.os.Build;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.Handler;
import android.os.Looper;
import java.util.function.Consumer;

/** One location fix from the system location service, never a cached or invented one. */
public final class TiloLocation {
    private static final long TIMEOUT_MS = 30_000;

    private TiloLocation() {}

    private static native void deliver(long token, boolean located, double lat, double lon, float accuracy, String reason);

    public static void locate(final Context context, final long token) {
        final LocationManager manager = context.getSystemService(LocationManager.class);
        if (manager == null) {
            deliver(token, false, 0, 0, 0, "this device has no location service");
            return;
        }
        if (!manager.isLocationEnabled()) {
            deliver(token, false, 0, 0, 0, "location is turned off on this device");
            return;
        }
        final String provider = provider(manager);
        if (provider == null) {
            deliver(token, false, 0, 0, 0, "no location provider is enabled");
            return;
        }
        final Handler main = new Handler(Looper.getMainLooper());
        main.post(new Runnable() {
            @Override
            public void run() {
                try {
                    request(context, manager, provider, main, token);
                } catch (SecurityException e) {
                    deliver(token, false, 0, 0, 0, "the app may not read the location");
                } catch (IllegalArgumentException e) {
                    deliver(token, false, 0, 0, 0, "the location provider " + provider + " is unavailable");
                }
            }
        });
    }

    private static String provider(LocationManager manager) {
        if (Build.VERSION.SDK_INT >= 31 && manager.hasProvider(LocationManager.FUSED_PROVIDER)
                && manager.isProviderEnabled(LocationManager.FUSED_PROVIDER)) {
            return LocationManager.FUSED_PROVIDER;
        }
        if (manager.isProviderEnabled(LocationManager.NETWORK_PROVIDER)) {
            return LocationManager.NETWORK_PROVIDER;
        }
        if (manager.isProviderEnabled(LocationManager.GPS_PROVIDER)) {
            return LocationManager.GPS_PROVIDER;
        }
        return null;
    }

    private static void located(long token, Location location) {
        if (location == null) {
            deliver(token, false, 0, 0, 0, "no location fix arrived");
        } else {
            deliver(token, true, location.getLatitude(), location.getLongitude(),
                    location.hasAccuracy() ? location.getAccuracy() : 0, null);
        }
    }

    @SuppressWarnings("deprecation")
    private static void request(Context context, final LocationManager manager, String provider, Handler main,
            final long token) {
        if (Build.VERSION.SDK_INT >= 30) {
            manager.getCurrentLocation(provider, new CancellationSignal(), context.getMainExecutor(),
                    new Consumer<Location>() {
                        @Override
                        public void accept(Location location) {
                            located(token, location);
                        }
                    });
            return;
        }
        final boolean[] answered = {false};
        final LocationListener listener = new LocationListener() {
            @Override
            public void onLocationChanged(Location location) {
                if (!answered[0]) {
                    answered[0] = true;
                    located(token, location);
                }
            }

            @Override
            public void onStatusChanged(String provider, int status, Bundle extras) {}

            @Override
            public void onProviderEnabled(String provider) {}

            @Override
            public void onProviderDisabled(String provider) {}
        };
        manager.requestSingleUpdate(provider, listener, Looper.getMainLooper());
        main.postDelayed(new Runnable() {
            @Override
            public void run() {
                if (!answered[0]) {
                    answered[0] = true;
                    manager.removeUpdates(listener);
                    located(token, null);
                }
            }
        }, TIMEOUT_MS);
    }
}
