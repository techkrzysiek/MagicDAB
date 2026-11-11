package pl.techkrzysiek.magicdab

import android.content.Context
import java.lang.ref.WeakReference
import java.net.InetAddress
import java.net.ServerSocket
import java.security.SecureRandom

/** What the activity, the service and the page share in the app's process. */
object MagicDab {
    const val TAG = "MagicDAB"

    /** The interface's dark background (web/src/styles.css): behind the page and the system bars. */
    const val BACKGROUND = 0xFF15161D.toInt()

    private const val DEFAULT_PORT = 28080

    /**
     * Admin login of the WebView (MAGICDAB_TOKEN of the server, crates/server/src/desktop.rs):
     * new with every start of the app, the same for every start of its server.
     */
    val token: String = ByteArray(16).also { SecureRandom().nextBytes(it) }.joinToString("") { "%02x".format(it) }

    /** The activity is on screen (the receiver is not stopped to save the battery then). */
    @Volatile
    var visible = false
        set(value) {
            field = value
            RadioService.instance?.idleCheck()
        }

    /** The page (for the media controls and the server's state). */
    @Volatile
    var page: WeakReference<MainActivity>? = null

    /**
     * Port of the server on 127.0.0.1. It stays: the page keeps its settings
     * (language, theme, station) per address.
     */
    fun port(context: Context): Int = prefs(context).getInt("port", DEFAULT_PORT)

    /** Another app has the port: a free one from now on. */
    fun movePort(context: Context) {
        val free = ServerSocket(0, 1, InetAddress.getByName("127.0.0.1")).use { it.localPort }
        prefs(context).edit().putInt("port", free).apply()
    }

    /** Asked once: the media notification shows without it, other notices do not. */
    fun firstTime(context: Context, what: String): Boolean {
        val p = prefs(context)
        if (p.getBoolean(what, false)) return false
        p.edit().putBoolean(what, true).apply()
        return true
    }

    /** A media action for the page (notification, headset, audio focus): play, stop, next, previous. */
    fun pageAction(action: String) {
        val a = page?.get() ?: return
        a.runOnUiThread { a.mediaAction(action) }
    }

    /** The server started, stopped or failed: the activity shows the page or why not. */
    fun serverChanged() {
        val a = page?.get() ?: return
        a.runOnUiThread { a.showServer() }
    }

    private fun prefs(context: Context) = context.getSharedPreferences("magicdab", Context.MODE_PRIVATE)
}
