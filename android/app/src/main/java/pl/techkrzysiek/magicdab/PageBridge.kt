package pl.techkrzysiek.magicdab

import android.webkit.JavascriptInterface

/**
 * `window.MagicDABAndroid` of the page (web/src/desktop.ts). Called on a
 * thread of the WebView, not the main one. Only the server's own page is ever
 * loaded in the WebView (other addresses open in the browser).
 */
class PageBridge(private val activity: MainActivity) {
    private val context = activity.applicationContext

    /** Admin login of the page (crates/server/src/desktop.rs). */
    @JavascriptInterface
    fun token(): String = MagicDab.token

    /** {"type": "playing" | "media" | "device", ...} */
    @JavascriptInterface
    fun postMessage(json: String) {
        activity.runOnUiThread { activity.onPageMessage(json) }
    }

    @JavascriptInterface
    fun saveStart(name: String, mime: String): Int = Downloads.start(context, name, mime)

    @JavascriptInterface
    fun saveChunk(id: Int, base64: String): Boolean = Downloads.chunk(context, id, base64)

    @JavascriptInterface
    fun saveEnd(id: Int, ok: Boolean) = Downloads.end(context, id, ok)
}
