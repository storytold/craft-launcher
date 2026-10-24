package ai.storyteller.artcraftlauncher;

import com.google.androidgamesdk.GameActivity;

import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.os.Bundle;

import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;

/**
 * GameActivity host for the Rust cdylib.
 *
 * GameActivity (not NativeActivity) is required: eframe's accesskit feature is a hard
 * `compile_error!` with android-native-activity, and NativeActivity has no soft keyboard / IME,
 * so a panel + text-entry app would be unusable.
 *
 * `android.app.lib_name` in AndroidManifest.xml is "main", which must match
 * `[lib] name = "main"` in the Rust crate.
 */
public class MainActivity extends GameActivity {

    static {
        System.loadLibrary("main");
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Write the package snapshot *before* super.onCreate(): GameActivity starts the native
        // thread from there, and the Rust detection reads this file exactly once at start-up. A
        // write after super loses the race on a fresh install, so every card says 可安装 until the
        // next launch. The Activity's context is already attached here, so PackageManager and
        // getFilesDir() are usable.
        writeInstalledApps();
        super.onCreate(savedInstanceState);
        watchLaunchRequests();
    }

    /**
     * Hand the installed suite to the Rust side.
     *
     * The launcher's own detection looks for desktop files, .app bundles and package-manager
     * records; none of those exist on Android, so every app read as "not installed". The Rust
     * crate has no JNI, and PackageManager is unreachable without it -- but this activity already
     * has both, so it enumerates the packages once and leaves the answer in a file that
     * `artcraft-launcher-native`'s Android detection reads.
     *
     * Format: one `<package id>\t<version name>` per line. A missing file simply means nothing is
     * installed, which is the correct answer on a first run.
     */
    private void writeInstalledApps() {
        StringBuilder out = new StringBuilder();
        PackageManager pm = getPackageManager();

        // Probe each id explicitly instead of trusting the listing. On this ROM
        // getInstalledPackages(0) answers with only this package even with QUERY_ALL_PACKAGES and a
        // <queries> block naming the rest, while a lookup by name is answered correctly.
        String[] suite = {
                "ai.storyteller.photocraft", "ai.storyteller.vectorcraft", "ai.storyteller.filmcraft",
                "ai.storyteller.lightcraft", "ai.storyteller.printcraft", "ai.storyteller.effectcraft",
                "ai.storyteller.designcraft", "ai.storyteller.wordcraft", "ai.storyteller.deckcraft",
                "ai.storyteller.soundcraft", "ai.storyteller.gridcraft", "ai.storyteller.cadcraft",
                "ai.storyteller.figcraft", "ai.storyteller.artcraftlauncher",
        };
        java.util.Set<String> seen = new java.util.LinkedHashSet<>();
        for (String name : suite) {
            try {
                PackageInfo info = pm.getPackageInfo(name, 0);
                seen.add(name + "\t" + (info.versionName == null ? "" : info.versionName));
            } catch (PackageManager.NameNotFoundException ignored) {
                // not installed
            }
        }
        for (PackageInfo info : pm.getInstalledPackages(0)) {
            String name = info.packageName;
            if (name == null || !name.startsWith("ai.storyteller.")) continue;
            seen.add(name + "\t" + (info.versionName == null ? "" : info.versionName));
        }
        for (String line : seen) out.append(line).append('\n');
        File file = new File(getFilesDir(), "installed-apps.txt");
        try (FileOutputStream w = new FileOutputStream(file)) {
            w.write(out.toString().getBytes(StandardCharsets.UTF_8));
        } catch (Exception e) {
            android.util.Log.w("ArtCraft", "could not write installed-apps.txt: " + e);
        }
    }

    /**
     * Starts apps the UI asks for.
     *
     * The UI is Rust and the workspace forbids `unsafe`, so it cannot reach PackageManager; it
     * writes the package id to this file and this watcher starts the app. Polling is fine here:
     * the launcher is in the foreground whenever a card is tapped, so the request is seen within
     * half a second and the file is removed as soon as it is handled.
     */
    private void watchLaunchRequests() {
        final java.io.File request = new java.io.File(getFilesDir(), "launch-request.txt");
        Thread t = new Thread(() -> {
            while (true) {
                try {
                    Thread.sleep(500);
                } catch (InterruptedException e) {
                    return;
                }
                if (!request.isFile()) continue;
                String pkg;
                try {
                    pkg = new String(java.nio.file.Files.readAllBytes(request.toPath()),
                                     java.nio.charset.StandardCharsets.UTF_8).trim();
                } catch (Exception e) {
                    continue;
                }
                //noinspection ResultOfMethodCallIgnored
                request.delete();
                if (pkg.isEmpty()) continue;
                final String target = pkg;
                runOnUiThread(() -> {
                    android.content.Intent i = getPackageManager().getLaunchIntentForPackage(target);
                    if (i == null) {
                        android.widget.Toast.makeText(this, target + " 没有可启动的界面",
                                android.widget.Toast.LENGTH_SHORT).show();
                        return;
                    }
                    i.addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK);
                    try {
                        startActivity(i);
                    } catch (Exception e) {
                        android.widget.Toast.makeText(this, "无法启动 " + target + ": " + e,
                                android.widget.Toast.LENGTH_LONG).show();
                    }
                });
            }
        });
        t.setDaemon(true);
        t.start();
    }
}
