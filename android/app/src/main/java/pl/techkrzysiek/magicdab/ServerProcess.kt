package pl.techkrzysiek.magicdab

import android.content.Context
import android.os.SystemClock
import android.util.Log
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

/**
 * The MagicDAB server: the Rust program built for Android (build/android/build.sh),
 * shipped as libmagicdab.so so that it is unpacked into the native library
 * folder, the one place an app may run programs from. A process of its own: a
 * crash of the receiver does not take the app with it, and it is simply
 * started again. [onChange] gets whether it answers, or why it ended.
 */
class ServerProcess(
    private val context: Context,
    /** The socket of UsbBridge (MAGICDAB_USB_SOCKET of the server). */
    private val usbSocket: String,
    private val onChange: (ready: Boolean, status: String?) -> Unit,
) {
    @Volatile
    private var wanted = false

    @Volatile
    private var process: Process? = null
    private var supervisor: Thread? = null
    private val lines = ArrayDeque<String>()

    fun start() {
        if (wanted) return
        wanted = true
        supervisor = thread(name = "magicdab-server") { supervise() }
    }

    /** SIGTERM (the server saves its settings and closes the tuner), after a while SIGKILL. */
    fun stop() {
        wanted = false
        supervisor?.interrupt()
        val p = process ?: return
        thread(name = "magicdab-stop") {
            p.destroy()
            if (!p.waitFor(5, TimeUnit.SECONDS)) p.destroyForcibly()
        }
    }

    /** The last lines of its log (shown when it does not start). */
    fun tail(): String = synchronized(lines) { lines.joinToString("\n") }

    private fun supervise() {
        var failures = 0
        while (wanted) {
            val started = SystemClock.elapsedRealtime()
            val code = runOnce()
            if (!wanted) break
            Log.w(MagicDab.TAG, "the server ended with code $code")
            onChange(false, context.getString(R.string.server_ended, code))
            // 2: it could not listen, another app has the port
            if (code == 2) MagicDab.movePort(context)
            failures = if (SystemClock.elapsedRealtime() - started > 60_000) 0 else failures + 1
            // 70: the receiver stopped and wants a new start at once (crates/server/src/main.rs)
            val pause = if (code == 70 || failures <= 1) 500L else minOf(60_000L, 1000L shl minOf(failures, 6))
            try {
                Thread.sleep(pause)
            } catch (e: InterruptedException) {
                break
            }
        }
    }

    private fun runOnce(): Int {
        val exe = File(context.applicationInfo.nativeLibraryDir, "libmagicdab.so")
        val data = File(context.filesDir, "data").apply { mkdirs() }
        val port = MagicDab.port(context)
        val builder = ProcessBuilder(
            exe.path,
            "--listen", "127.0.0.1:$port",
            "--data-dir", data.path,
            // (no such folder: the interface built into the program is used)
            "--web-root", File(context.filesDir, "web").path,
        ).directory(context.filesDir).redirectErrorStream(true)
        builder.environment().apply {
            put("MAGICDAB_TOKEN", MagicDab.token)
            // the stick, opened by the app (UsbBridge, crates/sdr/src/android_usb.rs)
            put("MAGICDAB_USB_SOCKET", usbSocket)
            // (debug builds: the demodulator's profile in the log while files/profile exists)
            if ((context.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0 && File(context.filesDir, "profile").exists()) {
                put("MAGICDAB_PROFILE", "1")
            }
            // (debug builds: every MSC symbol demodulated while files/allsymbols exists, for comparisons)
            if ((context.applicationInfo.flags and android.content.pm.ApplicationInfo.FLAG_DEBUGGABLE) != 0 && File(context.filesDir, "allsymbols").exists()) {
                put("MAGICDAB_ALL_SYMBOLS", "1")
            }
            // (the default temporary folder is not writable for apps)
            put("TMPDIR", context.cacheDir.path)
            put("HOME", context.filesDir.path)
        }
        val p = try {
            builder.start()
        } catch (e: Exception) {
            Log.e(MagicDab.TAG, "cannot start $exe", e)
            remember(e.toString())
            return -1
        }
        process = p
        // (stopped while it was starting)
        if (!wanted) p.destroy()
        // its log into logcat
        val reader = thread(name = "magicdab-log") {
            try {
                p.inputStream.bufferedReader().forEachLine { log(it) }
            } catch (e: Exception) {
            }
        }
        // the page can be shown once it answers
        thread(name = "magicdab-ready") {
            val until = SystemClock.elapsedRealtime() + 120_000
            try {
                while (wanted && p.isAlive && SystemClock.elapsedRealtime() < until) {
                    if (ping(port)) {
                        onChange(true, null)
                        return@thread
                    }
                    Thread.sleep(200)
                }
            } catch (e: InterruptedException) {
            }
        }
        val code = p.waitFor()
        process = null
        reader.join(1000)
        return code
    }

    private fun ping(port: Int): Boolean = try {
        val c = URL("http://127.0.0.1:$port/ping").openConnection() as HttpURLConnection
        c.connectTimeout = 500
        c.readTimeout = 1000
        try {
            c.responseCode == 200
        } finally {
            c.disconnect()
        }
    } catch (e: Exception) {
        false
    }

    /** "2026-10-30 04:08:40 WARN  [radio] text" at its level. */
    private fun log(line: String) {
        when (line.split(' ', limit = 4).getOrNull(2)) {
            "ERROR" -> Log.e(MagicDab.TAG, line)
            "WARN" -> Log.w(MagicDab.TAG, line)
            else -> Log.i(MagicDab.TAG, line)
        }
        remember(line)
    }

    private fun remember(line: String) {
        synchronized(lines) {
            lines.addLast(line)
            while (lines.size > 12) lines.removeFirst()
        }
    }
}
