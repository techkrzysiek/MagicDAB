package pl.techkrzysiek.magicdab

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.widget.Toast
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest
import kotlin.concurrent.thread

/**
 * A new version of the app (docs/android.md, "Updates"). The server finds it in
 * the signed update manifest (crates/server/src/update.rs, android-arm64) and the
 * page hands over its address and sha256 ("installApk"). The APK is downloaded,
 * checked against the sha256 and given to Android's installer for this package
 * only: Android takes only an APK of MagicDAB signed with the key of the installed
 * one, and asks the user. The update ends the app (the server, the playing): a
 * notification afterwards opens it again ([UpdateDone]).
 */
object Updater {
    private const val MAX_BYTES = 100L shl 20
    private const val CHANNEL = "updates"
    private const val NOTIFY_CONFIRM = 2
    private const val NOTIFY_DONE = 3

    private val main = Handler(Looper.getMainLooper())

    @Volatile
    private var busy = false

    fun install(context: Context, url: String, sha256: String, version: String) {
        if (busy || !url.startsWith("https://") || !Regex("[0-9a-fA-F]{64}").matches(sha256)) return
        busy = true
        toast(context, context.getString(R.string.update_downloading, version))
        thread(name = "magicdab-update") {
            val apk = File(context.cacheDir, "update.apk")
            try {
                download(url, apk)
                if (!sha256Of(apk).equals(sha256, ignoreCase = true)) throw IOException(context.getString(R.string.update_bad_sum))
                val installer = context.packageManager.packageInstaller
                val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
                // (an APK of another app fails here, whatever the page asked for)
                params.setAppPackageName(context.packageName)
                params.setSize(apk.length())
                val id = installer.createSession(params)
                installer.openSession(id).use { s ->
                    s.openWrite("magicdab.apk", 0, apk.length()).use { out ->
                        apk.inputStream().use { it.copyTo(out, 1 shl 16) }
                        s.fsync(out)
                    }
                    // (the new version shows a notice when it is in place: UpdateDone)
                    MagicDab.prefs(context).edit().putString("updatingTo", version).apply()
                    val flags = PendingIntent.FLAG_UPDATE_CURRENT or (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
                    val sender = PendingIntent.getBroadcast(context, id, Intent(context, UpdateReceiver::class.java).putExtra("version", version), flags).intentSender
                    s.commit(sender)
                }
            } catch (e: Exception) {
                Log.w(MagicDab.TAG, "update: $e")
                toast(context, context.getString(R.string.update_failed, e.message ?: e.toString()))
            } finally {
                apk.delete()
                busy = false
            }
        }
    }

    private fun download(url: String, to: File) {
        val c = URL(url).openConnection() as HttpURLConnection
        c.connectTimeout = 15_000
        c.readTimeout = 60_000
        try {
            if (c.responseCode != 200) throw IOException("HTTP ${c.responseCode}")
            if (c.contentLengthLong > MAX_BYTES) throw IOException("${c.contentLengthLong} bytes")
            var total = 0L
            c.inputStream.use { input ->
                to.outputStream().use { out ->
                    val buf = ByteArray(1 shl 16)
                    while (true) {
                        val n = input.read(buf)
                        if (n < 0) break
                        total += n
                        if (total > MAX_BYTES) throw IOException("over ${MAX_BYTES shr 20} MB")
                        out.write(buf, 0, n)
                    }
                }
            }
        } finally {
            c.disconnect()
        }
    }

    private fun sha256Of(file: File): String {
        val md = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buf = ByteArray(1 shl 16)
            while (true) {
                val n = input.read(buf)
                if (n < 0) break
                md.update(buf, 0, n)
            }
        }
        return md.digest().joinToString("") { "%02x".format(it) }
    }

    fun toast(context: Context, text: String) {
        main.post { Toast.makeText(context.applicationContext, text, Toast.LENGTH_LONG).show() }
    }

    /** Android's question "install?" when the app is not on screen (an activity may not start from the background). */
    fun askLater(context: Context, confirm: Intent, version: String) {
        val open = PendingIntent.getActivity(context, NOTIFY_CONFIRM, confirm, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        notify(context, NOTIFY_CONFIRM, context.getString(R.string.update_ready, version), context.getString(R.string.update_tap_install), open)
    }

    /** The new version is in place: the app ended with the update, a tap opens it again. */
    fun installed(context: Context, version: String) {
        val nm = context.getSystemService(NotificationManager::class.java)
        nm.cancel(NOTIFY_CONFIRM)
        val open = PendingIntent.getActivity(context, NOTIFY_DONE, Intent(context, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        notify(context, NOTIFY_DONE, context.getString(R.string.update_done, version), context.getString(R.string.update_tap_open), open)
    }

    private fun notify(context: Context, id: Int, title: String, text: String, open: PendingIntent) {
        val nm = context.getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, context.getString(R.string.channel_updates), NotificationManager.IMPORTANCE_DEFAULT))
        val n = Notification.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_magicdab)
            .setContentTitle(title)
            .setContentText(text)
            .setContentIntent(open)
            .setAutoCancel(true)
            .build()
        // (without the permission to notify, Android 13+ drops it)
        try {
            nm.notify(id, n)
        } catch (e: SecurityException) {
            Log.w(MagicDab.TAG, "update notice: $e")
        }
    }
}

/** The installer's answers: Android asks the user (on screen, else from a notification), then installs. */
class UpdateReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val version = intent.getStringExtra("version") ?: ""
        when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm = (
                    if (Build.VERSION.SDK_INT >= 33) {
                        intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                    } else {
                        @Suppress("DEPRECATION")
                        intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT)
                    }
                    ) ?: return
                confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                if (MagicDab.visible) {
                    try {
                        context.startActivity(confirm)
                        return
                    } catch (e: Exception) {
                        Log.w(MagicDab.TAG, "update: $e")
                    }
                }
                Updater.askLater(context, confirm, version)
            }
            PackageInstaller.STATUS_SUCCESS -> {}
            else -> {
                MagicDab.prefs(context).edit().remove("updatingTo").apply()
                val msg = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "?"
                Log.w(MagicDab.TAG, "update: $msg")
                // (STATUS_FAILURE_ABORTED: the user said no)
                if (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, 0) != PackageInstaller.STATUS_FAILURE_ABORTED) {
                    Updater.toast(context, context.getString(R.string.update_failed, msg))
                }
            }
        }
    }
}

/** The app was replaced (MY_PACKAGE_REPLACED, the new version runs this): after an update of its own, a notice. */
class UpdateDone : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_MY_PACKAGE_REPLACED) return
        val prefs = MagicDab.prefs(context)
        val wanted = prefs.getString("updatingTo", null) ?: return
        prefs.edit().remove("updatingTo").apply()
        val now = try {
            context.packageManager.getPackageInfo(context.packageName, 0).versionName
        } catch (e: Exception) {
            null
        }
        Updater.installed(context, now ?: wanted)
    }
}
