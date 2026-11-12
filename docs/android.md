# MagicDAB for Android

The Android app is MagicDAB on a phone or tablet: DAB/DAB+ only (no FM tuner,
no DAB/FM linking), an RTL-SDR stick on USB OTG, started like any app. It is
the same receiver as on Linux and Windows - the Rust server and the React
interface of this repository - wrapped in a small Kotlin app (`android/`).

It is a receiver for the person holding the phone, not a server: the stick on
its USB is the only input, the interface is only on the phone itself
(127.0.0.1), and nothing is made public - no tunnel, no listing on the map of
servers, no chat, no first-run wizard (`Settings::phone_only`).

## Architecture

```
 MainActivity: WebView ──── http + WebSocket, 127.0.0.1:28080 ─────┐
   │  the web interface (the sound is the server's, below)        │
   │  window.MagicDABAndroid: token, play state, station,         │
   │  tuner state, files                                          │
 RadioService (foreground service, media playback)                │
   │  notification + MediaSession, audio focus, wake lock         │
   ├── starts, restarts, stops ──> libmagicdab.so: the server, ───┘
   │                                a process of its own, librtlsdr.so,
   │                                the sound to AAudio (speaker.rs)
   │                                         │ "open" (local socket)
   └── UsbBridge: UsbManager.openDevice ──────┘ the stick's fd (SCM_RIGHTS)
                    │
                  USB OTG ── RTL-SDR stick
```

### The server: a program in the app, not a JNI library

The server is the ordinary `magicdab` program built for Android
(`aarch64-linux-android` etc.), shipped in the APK as
`lib/<abi>/libmagicdab.so` and started by the app with `ProcessBuilder`
(`android/app/src/main/java/pl/techkrzysiek/magicdab/ServerProcess.kt`):

```
libmagicdab.so --listen 127.0.0.1:<port> --data-dir <files>/data --web-root <files>/web
```

Why a program and not a library loaded through JNI:

- **No restructuring of the server.** It is a binary crate (`main.rs` with
  its modules, argument parsing, signal handling, `process::exit` on fatal
  errors, re-executing itself after updates). As a library it would need a
  lib/bin split, a stop function, and no `exit` anywhere - big changes in a
  crate others work on in parallel. As a program it runs as it is: the
  Android specifics are a few `cfg!(target_os = "android")`.
- **A crash of the receiver does not take the app with it.** A panic in the
  receiver thread or a crash in a C codec (faad2 is fed data from the air) ends
  only the server; the app starts it again (exit code 70 asks for that,
  `update::supervised`) and the page reconnects by itself.
- **Clean start and stop.** SIGTERM saves the settings and closes the tuner as
  `docker stop` does; a fresh process means no leftover state.
- **The log** goes to logcat (the app reads stdout/stderr) and to
  `data/magicdab.log`, shown in the Log view after a restart.

What it costs, and how it is handled:

- The program must exist as a file: the APK is built with
  `packaging.jniLibs.useLegacyPackaging = true` (= `extractNativeLibs`), and
  the name must be `lib*.so` for the installer to unpack it. The native library
  folder is the one place an app may run programs from (Android 10+ forbids
  running files from the app's writable folders, W^X).
- Child processes are "phantom processes" for Android 12+, which kills them
  when an app in the background uses too much CPU or when there are more than
  32 of them on the whole phone. The foreground service exempts the app from
  the first; the second is rare (Termux users), and the app restarts the
  server if it happens.
- Phones with 16 KB memory pages (Android 15+) need 16 KB aligned ELF
  segments: the build links with `-z max-page-size=16384`.
- Executing a program that comes inside the APK is allowed by Google Play
  (Syncthing and Orbot did the same); only downloaded code is not.

### The interface: the existing web UI in a WebView

The activity shows `http://127.0.0.1:<port>/` in a WebView. Nothing of the
interface is duplicated in Kotlin; the page's mobile layout (below 760 px) is
used on phones, the desktop one on tablets in landscape.

- **Admin without a password**, like the window of the Windows application:
  the app makes a random token per process, gives it to the server
  (`MAGICDAB_TOKEN`, read only on Android, `crates/server/src/desktop.rs`) and to
  the page (`MagicDABAndroid.token()`); the page logs in with it
  (`web/src/desktop.ts`). The token outlives a restart of the server, so the
  page logs in again after one.
- **Port**: 28080 on 127.0.0.1 only (not reachable from the network). It stays
  the same because the page keeps its settings (language, theme, station) per
  address; if another app has it (the server exits with code 2), the app picks
  a free one once and keeps that.
- **Bridge** `window.MagicDABAndroid` (`PageBridge.kt`): the token, messages
  from the page (`playing`, `media` with the station, DLS, multiplex and logo,
  `device` with the tuner's state) and saving files. Media actions go back to
  the page as `magicdab-media` events (`web/src/audio/mediasession.ts`).
- **Sound**: the page plays as in a browser (Web Audio, AudioWorklet). The
  app's WebView allows playback without a tap (notification, headset, end of
  a call). In the app the page asks for PCM (`codec` default): next to the
  server it spares the phone encoding and decoding Opus.
- **Back** closes the page's dialogs, menus and the chat first
  (`window.magicdabBack`, `web/src/android.ts`), then moves the app to the
  background - it keeps playing.
- **Files** the page saves (CSV exports, recordings) come through the bridge in
  parts into `Downloads/MagicDAB` (a WebView saves no downloads).
- **Links** to other addresses open in the browser; `<input type="file">` (the
  transmitter list for TII) opens the system file picker.
- **Settings** in the app: no FM section (`fmAvailable` from the server); the
  updates section has no manifest address and no "by itself" (see "Updates").

### Playing with the screen off: the foreground service

`RadioService` is a foreground service of type `mediaPlayback`. It starts when
the activity comes on screen, and:

- runs the server (`ServerProcess`), restarting it when it ends;
- shows the notification: a `MediaStyle` notification with a `MediaSession`
  (station, DLS, multiplex, the station logo when the page has one in PNG/JPEG;
  play/stop and quit; the lock screen, Bluetooth and headset buttons; next and
  previous station);
- holds the audio focus while the page plays (a call or another player stops
  it, the end of a call starts it again; Android ducks it for short sounds),
  stops when headphones are pulled out, and holds a partial wake lock (the
  demodulator needs the processor with the screen off);
- stops itself and the server 3 minutes after the last listening when the
  app is out of sight (the receiver demodulates all the time: battery); the
  app starts it again when it comes back, and the page reconnects;
- quits everything with the notification's Quit or when the app is swiped
  away from the recent apps (the page, and so the sound, is gone then).

The page and its sound live in the activity, so the activity is never
recreated: `configChanges` covers rotation, dark mode, font size, keyboards,
multi-window; if the WebView's renderer is killed, a new WebView loads the page.

### The stick: opened by the app, used by the server

Apps cannot open USB devices with librtlsdr/libusb the way Linux programs do
(no access to `/dev/bus/usb`): Android opens the device for an app the user
allowed, and the app hands the open device's file descriptor to the server.

- `UsbBridge.kt`: a local socket (abstract name `magicdab-usb-<token>`, only
  the app's own uid is served) in the service. The server asks `open`: the app
  finds the stick (`res/xml/rtlsdr_devices.xml`), asks Android for the
  permission when it has none (`requestPermission`, the system dialog; only
  while the app is on screen, not asked again after a "no" until the stick is
  plugged in again or the app is opened again), opens it and sends the fd
  (`setFileDescriptorsForSend`). `list` gives the names of the sticks plugged
  in (the settings).
- The server (`crates/sdr/src/android_usb.rs`) receives the fd (`recvmsg`,
  `SCM_RIGHTS`; a child process inherits none) and opens it with
  `rtlsdr_open_fd`, a small patch of librtlsdr 2.0.3
  (`build/android/rtlsdr-open-fd.patch`: `LIBUSB_OPTION_NO_DEVICE_DISCOVERY` and
  `libusb_wrap_sys_device` instead of the enumeration). From there the RTL-SDR
  code is the one of Linux: gain table of the actual tuner (R828D of the
  RTL-SDR Blog V4), bias-T, PPM, AGC.
- A stick plugged in opens MagicDAB (`USB_DEVICE_ATTACHED`; "always open" in
  Android's question gives the permission for good). The server tries to open
  its input every 2 s, so a stick plugged in or allowed is used at once.

### Server behaviour on Android

All `cfg!(target_os = "android")` (Linux and Windows unchanged):

- the stick only, nothing public (`Settings::phone_only`, applied to the
  settings as loaded and to every change of them): the input is the RTL-SDR
  stick, the tunnel, the listing on the map, the chat, ETI/EDI channel maps,
  listeners' own multiplexes, FM and linking stay off, no first-run wizard;
  the interface hides their settings, the logout and the keyboard shortcuts;
- a phone with a battery (`Settings::android`): no idle scans, no background
  DLS/slides of every station (the data services, SPI and EPG, are decoded as
  on a server: measured, about 4 % of a core and no difference in current);
- the FM tuner is never started (serial port and ALSA are not there for apps,
  `fmradio::AVAILABLE`), and the interface hides its settings;
- the log also goes into `data/magicdab.log`;
- a panic of the receiver exits with code 70 (the app restarts the server)
  instead of re-executing the program;
- updates: the platform is `android-arm64`; the server checks the manifest
  as everywhere (at each start of the app, then every 12 h) but never
  installs: the page hands the APK to the app (see "Updates").

### Battery

Measured on a phone with an RTL-SDR Blog V4 (a USB hub and a network adapter
on the same port), screen off, a station of 192 kbit/s playing:

| | mA from the battery |
|---|---|
| app stopped (the stick idle) | 666 |
| playing | 805 |
| playing, every MSC symbol demodulated (as before) | 812 |

So the receiver adds about 140 mA; most of it is the stick streaming and the
demodulator. What was done:

- the demodulator works out only the OFDM symbols whose bits are used (the
  FIC and the sub-channels decoded: one station of 192 kbit/s lies in 4 of the
  18 symbols of a CIF; all of them for an ETI recording or the spectrum view):
  the per-carrier detection went from 25 % to 9 % of a core (the clock of the
  core then drops, hence the small difference in current);
- a hidden page (screen off) takes in the signal and statistics messages only
  every 5 s and disconnects its loudness meter.

- the server plays the sound itself (`crates/server/src/speaker.rs`, AAudio
  in power saving mode, so the system plays it from its deep buffer): the
  page receives no audio and decodes nothing. Before, the WebView's sound (the
  PCM over the WebSocket, its AudioWorklet resampler, Chromium's own output)
  took about 30 % of a little core with the screen off. The same page in
  desktop Chromium: 13 % of a core with Opus, 5 % with the program's sound.

Measured with it (a station of 192 kbit/s on the phone's speaker, screen off,
the phone on Wi-Fi, 90 s each, in the order of the table):

| | mA | all cores, % of one | WebView renderer | app process | server |
|---|---|---|---|---|---|
| the server's sound | 710, 723, 730 | 67-77 | 1 | 0.5 | 55 |
| the page's sound (PCM) | 726, 741 | 98 | 29 | 8.5 | 49 |
| nothing played (decoding only) | 663 | 68 | | | |

A quarter of a core less, but only about 15 mA: like the demodulator change,
the little cores at their lowest clock draw little. The stick and the
receiver's own work remain the bulk.

### Other ways considered

- **The server as a JNI library** in the app's process: see above; it would
  matter for native USB (the USB file descriptor is in the app's process),
  which the program can get over a Unix socket instead (below).
- **A player in the service** (MediaPlayer/ExoPlayer on a stream of the
  server) instead of the server's own output: a second audio path with its
  own buffering, and an encoded stream between the two. The server's output
  (AAudio, loaded at run time) is the one the Windows window and a browser on
  the same computer use too; the page still drives it (play, stop, station,
  volume, buffer), and the audio focus stays with the service.
- **The browser instead of a WebView**: a tab in the background is throttled
  and killed, it cannot be logged in without the password, it has no
  foreground service.
- **The "RTL-SDR driver" app** (rtl_tcp_andro) serving the stick as rtl_tcp on
  the phone: the first version did so; one more app to install, 4 MB/s
  through a socket, and no gain table of the actual tuner.

### Updates

The release's APK is in the signed update manifest (`latest.json`, the same
Ed25519 signature as the other packages) as `android-arm64`: `android.yml`
adds the entry after it uploads the APK and signs the manifest again
(`UPDATE_SIGNING_KEY`). Only release builds know the manifest (`UPDATE_URL`
of a tag build); test builds check nothing.

- The server checks the manifest and its signature (`crates/server/src/update.rs`)
  and tells the page the new version, its address and sha256: the banner
  "MagicDAB … is available" and Settings > Internet > Updates.
- "Update now" sends `installApk` to the app (`Updater.kt`): it downloads the
  APK (https, at most 100 MB) into its cache, checks the sha256 and hands it
  to Android's `PackageInstaller` for this package only (`setAppPackageName`).
  Android installs only an APK of MagicDAB signed with the key of the installed
  app (the release keystore) and asks the user every time; the first time
  it also asks to allow MagicDAB to install apps (`REQUEST_INSTALL_PACKAGES`).
- The download shows how far it is, with its speed, in the banner, in
  Settings and in a quiet notification ("Update downloads"); then "Checking
  the download…", "Android asks…", or why it failed (with the button again).
- Android's question opens at once when the app is on screen; when it is not
  (an activity may not start from the background), a notification "ready to
  install" opens it.
- The update ends the app (the server, the playing, the notification). The
  new version shows "MagicDAB updated to …" (`MY_PACKAGE_REPLACED`, only after
  an update of its own); a tap opens it.
- An app of 0.3.26 or older has no updater: that one update is by hand, the
  new APK over the old app, with the settings kept.

### Data

`/data/data/pl.techkrzysiek.magicdab/files/data` (the server's data folder:
settings, service list, logos, slides, logs) - private to the app, removed
with it, not in Android backups (`allowBackup="false"`). The WebView keeps the
page's own settings (localStorage) per address.

## Building

The server for phones needs the Android NDK (r27 or newer) with the codecs
(opus 1.5.2, faad2 2.11.2 - the versions of the Windows package) built as
static libraries for each ABI with the NDK's CMake toolchain; the app needs
the Android SDK (platform 36), JDK 17+ and Gradle 8.14.3, and librtlsdr 2.0.3
(patched, with libusb 1.0.27 linked in) as `librtlsdr.so` next to the server.
One script does all of it: `build/android/build.sh`.

- **CI** (`.github/workflows/android.yml`, Actions tab or a `v*` tag): the
  runner's SDK and NDK, arm64-v8a and x86_64 (emulators), debug and release
  APKs as artifacts; for tags the signed release APK goes on the release page
  made by `release.yml`. Signing: the secrets `ANDROID_KEYSTORE` (base64 of a
  keystore, e.g. `keytool -genkeypair -v -keystore magicdab.keystore -alias
  magicdab -keyalg RSA -keysize 4096 -validity 20000`), `ANDROID_KEYSTORE_PASSWORD`,
  `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD`. Keep the keystore safe: updates
  of an installed app must be signed with the same key. Without it the
  release APK is unsigned and the debug APK is signed with a key of that run
  (a newer one installs only after uninstalling the older).
- **Docker** (`build/android.sh`, no SDK/NDK on the computer): an image with
  the NDK, SDK and Gradle (about 8 GB of disk with the caches) runs the same
  script; `dist/android/MagicDAB-<version>-android-debug.apk`. `JOBS=1` next to
  a running receiver.
- **Locally** with the SDK and NDK: `ANDROID_NDK_HOME=... ANDROID_HOME=...
  build/android/build.sh` (`ABIS`, `APK=release`, `SKIP_WEB=1` - see the
  script). Android Studio opens `android/`; its builds need the server in
  `android/app/src/main/jniLibs` first (`APK= build/android/build.sh`; the
  Gradle build stops with a message without it).

Versions: `0.3.<commits>-dev` (or the tag of a release), version code =
the number of commits. Application id `pl.techkrzysiek.magicdab` (it cannot
change once the app is published).
minSdk 26 (Android 8: notification channels, adaptive icons, audio focus
requests), targetSdk 36. No library dependencies besides Kotlin's standard
library (no AndroidX).

## What was verified

On a phone with Android 16 and an RTL-SDR Blog V4, debug APK built in Docker:

- the stick opened by the app (permission dialog, "always open"), band scan,
  playback of DAB+ stations, with the screen off for half an hour without a dropout;
- Back to the background and playing on; the notification's and headset's
  play, stop and next station; the server killed: started again in about a
  second, the stick opened again, the page reconnected and played on;
- the interface on the phone: the channel on the Multiplex tab, the settings
  without what a phone does not have;
- the demodulator change (symbols): the same audio errors (none) on an IQ
  recording of the phone with and without it (`uffplay`, `MAGICDAB_ALL_SYMBOLS=1`).
- the server's sound output: AAudio opens from the server's process (a media
  stream of the app's uid, deep buffer, about 230 ms), starts without a
  dropout (the output's buffer is filled with silence first, playback starts
  from a full buffer), plays on with the screen off; the notification's
  pause closes the stream and play opens it again.

Debug builds only: `files/profile` (run-as) puts the demodulator's profile in
the log, `files/allsymbols` demodulates every symbol (comparisons).

## Next steps

- The update from the app on a phone (download, Android's question, the
  notice afterwards): not tried yet.
- The server's sound output with a headset unplugged and plugged in again
  (AAudio disconnects the stream: the server opens a new one; not tried).
- Re-plugging the stick while playing (not tried: the stick was out of reach).
- A note for phones that kill background apps (dontkillmyapp.com).
- The station logo also from SVG (render it in the page to PNG).
- License texts in the APK (About), F-Droid metadata (reproducible build,
  no Play services - there are none).
