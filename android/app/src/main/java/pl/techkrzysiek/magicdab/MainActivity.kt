package pl.techkrzysiek.magicdab

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.drawable.AnimatedVectorDrawable
import android.hardware.usb.UsbManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.util.TypedValue
import android.graphics.Typeface
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowInsets
import android.webkit.RenderProcessGoneDetail
import android.webkit.ValueCallback
import android.webkit.WebChromeClient
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import android.widget.Toast
import android.window.OnBackInvokedDispatcher
import org.json.JSONObject
import java.lang.ref.WeakReference

/**
 * The receiver's web interface in a WebView, from the server on this phone
 * (http://127.0.0.1:<port>/), logged in as admin. The page plays the sound
 * itself (Web Audio), so it must live on: the activity is never made anew
 * (configChanges in the manifest), Back moves the app to the background, and
 * the service keeps the process running while the page plays.
 */
class MainActivity : Activity() {
    companion object {
        private const val REQUEST_FILE = 2
        private const val REQUEST_NOTIFICATIONS = 3

        /** The cover stays this long at least: the hat drawn, the star out (loading_logo). */
        private const val INTRO_MS = 1300L
    }

    private val main = Handler(Looper.getMainLooper())
    private lateinit var root: FrameLayout

    /** Over the page until it is there (its first state, `ready`): the icon's
     *  animation while the server starts and the page loads, and a line on
     *  why when it does not. */
    private lateinit var cover: FrameLayout
    private lateinit var coverLogo: ImageView
    private lateinit var coverName: TextView
    private lateinit var coverText: TextView
    /** The name and the line below the icon. */
    private lateinit var coverInfo: LinearLayout
    /** The page is being shown (hideCover): it stays visible. */
    private var revealing = false
    private var coverSince = 0L
    private var web: WebView? = null

    /** The port of the page loaded (null: none yet, or it failed). */
    private var shownPort: Int? = null
    private var pageFailed = false
    private var files: ValueCallback<Array<Uri>>? = null

    private var usbRegistered = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        root = FrameLayout(this)
        root.setBackgroundColor(MagicDab.BACKGROUND)
        val dp = resources.displayMetrics.density
        cover = FrameLayout(this).apply {
            setBackgroundColor(MagicDab.BACKGROUND)
            // (taps do not reach the page under it)
            isClickable = true
        }
        // in the middle, where the system's start screen had the tile (splash_logo)
        coverLogo = ImageView(this).apply { setImageResource(R.drawable.loading_logo) }
        cover.addView(coverLogo, FrameLayout.LayoutParams((96 * dp).toInt(), (96 * dp).toInt(), Gravity.CENTER))
        // the name as in the interface's rail (web/src/styles.css .brand: the
        // accent, extra bold), coming up with the hat
        coverName = TextView(this).apply {
            text = getString(R.string.app_name)
            gravity = Gravity.CENTER_HORIZONTAL
            setTextColor(0xFFA68BFF.toInt())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 24f)
            typeface = if (Build.VERSION.SDK_INT >= 28) Typeface.create(Typeface.DEFAULT, 800, false) else Typeface.DEFAULT_BOLD
            letterSpacing = 0.03f
        }
        coverText = TextView(this).apply {
            gravity = Gravity.CENTER_HORIZONTAL
            setTextColor(0xFFA9ACBE.toInt())
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
            alpha = 0f
        }
        coverInfo = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(coverName, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
            addView(
                coverText,
                LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { topMargin = (14 * dp).toInt() },
            )
        }
        cover.addView(coverInfo, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        // below the icon
        cover.addOnLayoutChangeListener { v, _, top, _, bottom, _, oldTop, _, oldBottom ->
            if (bottom - top != oldBottom - oldTop) {
                val pad = (24 * dp).toInt()
                coverInfo.setPadding(pad, (bottom - top) / 2 + (64 * dp).toInt(), pad, pad)
                v.post { coverInfo.requestLayout() }
            }
        }
        root.addView(cover, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        showCover()
        // the system's start screen goes at once (not faded out to nothing):
        // the cover with the same tile is already drawn under it
        if (Build.VERSION.SDK_INT >= 31) splashScreen.setOnExitAnimationListener { it.remove() }
        setContentView(root)
        root.setOnApplyWindowInsetsListener { v, insets -> applyInsets(v, insets) }
        WebView.setWebContentsDebuggingEnabled((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0)
        createWebView()
        MagicDab.page = WeakReference(this)
        if (Build.VERSION.SDK_INT >= 33) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT) { back() }
        }
        usbAttached(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        usbAttached(intent)
    }

    override fun onStart() {
        super.onStart()
        MagicDab.visible = true
        registerUsb()
        if (!RadioService.started) {
            // the first start, or the receiver stopped while nobody listened
            RadioService.start(this)
            if (Sticks.first(this) == null) Toast.makeText(this, R.string.plug_stick, Toast.LENGTH_LONG).show()
        }
        // (the stick's permission may be asked for again)
        RadioService.instance?.usbAgain()
        showServer()
    }

    override fun onStop() {
        MagicDab.visible = false
        unregisterUsb()
        super.onStop()
    }

    override fun onDestroy() {
        if (MagicDab.page?.get() === this) MagicDab.page = null
        main.removeCallbacksAndMessages(null)
        web?.let {
            root.removeView(it)
            it.destroy()
        }
        web = null
        // the page and its sound are gone, so is the receiver
        if (isFinishing) RadioService.instance?.quit(keepApp = true)
        super.onDestroy()
    }

    // ---------------------------------------------------------------- the page

    /** The page once the server answers, or why it does not (MagicDab.serverChanged). */
    fun showServer() {
        val service = RadioService.instance
        val port = MagicDab.port(this)
        if (service != null && service.ready) {
            if (shownPort != port) load(port)
        } else if (shownPort == null) {
            val status = service?.status
            if (service != null && status != null) {
                coverStatus(getString(R.string.server_problem, status, service.serverTail()), now = true)
            } else {
                coverStatus(getString(R.string.starting))
            }
        }
        // (a page already shown connects again by itself)
    }

    /** The cover with its animation from the start, the text after a while
     *  (a quick start shows only the icon). */
    private fun showCover(intro: Boolean = true) {
        cover.animate().cancel()
        cover.alpha = 1f
        cover.visibility = View.VISIBLE
        // the page draws nothing under it: its first frames hold up the render
        // thread for over 100 ms each, and the icon's animation with them
        web?.visibility = View.INVISIBLE
        revealing = false
        coverSince = SystemClock.uptimeMillis()
        (coverLogo.drawable as? AnimatedVectorDrawable)?.reset()
        coverName.animate().cancel()
        coverName.alpha = 0f
        coverLogo.animate().cancel()
        coverLogo.alpha = 1f
        coverInfo.animate().cancel()
        coverInfo.alpha = 1f
        if (intro) startIntro()
    }

    /** The icon's animation and the name. */
    private fun startIntro() {
        coverSince = SystemClock.uptimeMillis()
        (coverLogo.drawable as? AnimatedVectorDrawable)?.start()
        coverName.translationY = 8 * resources.displayMetrics.density
        coverName.animate().alpha(1f).translationY(0f).setStartDelay(250).setDuration(450)
        coverStatus(getString(R.string.starting))
    }

    private fun coverStatus(text: String, now: Boolean = false) {
        if (coverText.text.toString() == text) {
            if (now) {
                coverText.animate().cancel()
                coverText.alpha = 1f
            }
            return
        }
        coverText.text = text
        coverText.animate().cancel()
        if (now) {
            coverText.alpha = 1f
        } else {
            coverText.alpha = 0f
            coverText.animate().alpha(1f).setStartDelay(1500).setDuration(400)
        }
    }

    /** The page is there (or it is taking too long to say so): the cover fades
     *  away, once the star has come out of the hat (loading_logo). */
    private fun hideCover() {
        if (cover.visibility != View.VISIBLE || pageFailed) return
        val wait = INTRO_MS - (SystemClock.uptimeMillis() - coverSince)
        if (wait > 0) {
            main.postDelayed({ hideCover() }, wait)
            return
        }
        val w = web
        if (w == null || revealing) {
            if (w == null) fadeCover()
            return
        }
        // the icon and the name fade first; then the page's first frame is drawn
        // under the bare cover (it holds up the render thread for a quarter of
        // a second: nothing moves then) and the cover fades into the page
        revealing = true
        val reveal = {
            if (web === w && revealing) {
                w.visibility = View.VISIBLE
                var faded = false
                val fade = {
                    if (!faded && revealing) {
                        faded = true
                        fadeCover()
                    }
                }
                w.postVisualStateCallback(0, object : WebView.VisualStateCallback() {
                    override fun onComplete(requestId: Long) {
                        // (one more frame: the page's is on the screen)
                        main.postDelayed(fade, 50)
                    }
                })
                main.postDelayed(fade, 1500)
            }
        }
        coverInfo.animate().alpha(0f).setStartDelay(0).setDuration(180)
        // (the page only once the icon is gone: two frames after its fade)
        coverLogo.animate().alpha(0f).setStartDelay(0).setDuration(180).withEndAction { cover.postOnAnimationDelayed(reveal, 34) }
    }

    private fun fadeCover() {
        cover.animate().alpha(0f).setStartDelay(0).setDuration(250).withLayer().withEndAction {
            cover.visibility = View.GONE
            (coverLogo.drawable as? AnimatedVectorDrawable)?.stop()
        }
    }

    private fun load(port: Int) {
        shownPort = port
        pageFailed = false
        web?.loadUrl("http://127.0.0.1:$port/")
    }

    /** Play, stop, next, previous: the notification, a headset, the audio focus (web/src/audio/mediasession.ts). */
    fun mediaAction(action: String) {
        web?.evaluateJavascript("window.dispatchEvent(new CustomEvent('magicdab-media', {detail: '$action'}))", null)
    }

    /** How far an update of the app is (Updater.progress, web/src/android.ts). */
    fun apkProgress(json: String) {
        web?.evaluateJavascript("window.dispatchEvent(new CustomEvent('magicdab-apk', {detail: $json}))", null)
    }

    /** From the page (web/src/android.ts, web/src/audio/mediasession.ts). */
    fun onPageMessage(json: String) {
        val m = try {
            JSONObject(json)
        } catch (e: Exception) {
            return
        }
        when (m.optString("type")) {
            // its first state drawn (web/src/android.ts)
            "ready" -> hideCover()
            "playing" -> {
                val on = m.optBoolean("value")
                RadioService.instance?.setPlaying(on)
                if (on) askForNotifications()
            }
            "scanning" -> RadioService.instance?.setScanning(m.optBoolean("value"))
            "media" -> RadioService.instance?.setMedia(
                m.optString("title"),
                m.optString("artist"),
                m.optString("album"),
                m.optString("art").ifEmpty { null },
            )
            // a new version of the app (the server checked the signed manifest): Updater
            "installApk" -> Updater.install(this, m.optString("url"), m.optString("sha256"), m.optString("version"))
        }
    }

    @SuppressLint("SetJavaScriptEnabled", "JavascriptInterface")
    private fun createWebView() {
        val w = WebView(this)
        w.setBackgroundColor(MagicDab.BACKGROUND)
        w.settings.javaScriptEnabled = true
        w.settings.domStorageEnabled = true
        // the notification, a headset or the end of a call start the sound too, not only a tap
        w.settings.mediaPlaybackRequiresUserGesture = false
        w.settings.allowFileAccess = false
        w.settings.allowContentAccess = false
        w.addJavascriptInterface(PageBridge(this), "MagicDABAndroid")
        w.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean {
                if (isServer(request.url)) return false
                // links elsewhere (broadcasters, maps, the source code) open in the browser
                openExternal(request.url)
                return true
            }

            override fun onPageFinished(view: WebView, url: String?) {
                // (the page says when it has its first state: this is in case it does not)
                if (view === web && !pageFailed && isServer(url?.let { Uri.parse(it) })) {
                    main.postDelayed({ if (view === web) hideCover() }, 8000)
                }
            }

            override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
                if (view !== web || !request.isForMainFrame) return
                Log.w(MagicDab.TAG, "the page: ${error.description}")
                pageFailed = true
                shownPort = null
                showCover()
                main.postDelayed({ showServer() }, 2000)
            }

            override fun onRenderProcessGone(view: WebView, detail: RenderProcessGoneDetail): Boolean {
                // the page's renderer crashed or was ended for memory: a new WebView
                Log.w(MagicDab.TAG, "the page's renderer is gone (crash: ${detail.didCrash()})")
                if (view === web) {
                    root.removeView(view)
                    view.destroy()
                    web = null
                    shownPort = null
                    showCover()
                    createWebView()
                    showServer()
                }
                return true
            }
        }
        w.webChromeClient = object : WebChromeClient() {
            // <input type="file">: the transmitter list (TII) and the like
            override fun onShowFileChooser(
                view: WebView,
                callback: ValueCallback<Array<Uri>>,
                params: WebChromeClient.FileChooserParams,
            ): Boolean {
                files?.onReceiveValue(null)
                files = callback
                return try {
                    startActivityForResult(params.createIntent(), REQUEST_FILE)
                    true
                } catch (e: ActivityNotFoundException) {
                    files = null
                    false
                }
            }
        }
        if (cover.visibility == View.VISIBLE) w.visibility = View.INVISIBLE
        root.addView(w, 0, FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        web = w
    }

    private fun isServer(uri: Uri?): Boolean =
        uri != null && uri.scheme == "http" && uri.host == "127.0.0.1" && uri.port == MagicDab.port(this)

    private fun openExternal(uri: Uri) {
        if (uri.scheme !in setOf("http", "https", "mailto", "geo")) return
        try {
            startActivity(Intent(Intent.ACTION_VIEW, uri))
        } catch (e: ActivityNotFoundException) {
            Toast.makeText(this, R.string.no_app, Toast.LENGTH_SHORT).show()
        }
    }

    /** Edge to edge (always from Android 15): the page between the system bars and above the keyboard. */
    @Suppress("DEPRECATION")
    private fun applyInsets(v: View, insets: WindowInsets): WindowInsets {
        if (Build.VERSION.SDK_INT >= 30) {
            val i = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout() or WindowInsets.Type.ime())
            v.setPadding(i.left, i.top, i.right, i.bottom)
            centerCover(i.left, i.top, i.right, i.bottom)
            return WindowInsets.CONSUMED
        }
        v.setPadding(insets.systemWindowInsetLeft, insets.systemWindowInsetTop, insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
        centerCover(insets.systemWindowInsetLeft, insets.systemWindowInsetTop, insets.systemWindowInsetRight, insets.systemWindowInsetBottom)
        return insets.consumeSystemWindowInsets()
    }

    /** The cover's icon in the middle of the screen, where the system's start
     *  screen had it, not in the middle of the space between the bars. */
    private fun centerCover(left: Int, top: Int, right: Int, bottom: Int) {
        // (the icon and its text, not the cover: moved, it would bare a strip of the page)
        for (v in listOf(coverLogo, coverInfo)) {
            v.translationX = (right - left) / 2f
            v.translationY = (bottom - top) / 2f
        }
    }

    /** Back: the page first closes its dialogs and menus (web/src/android.ts); then the app goes to the background and plays on. */
    private fun back() {
        val w = web
        if (w == null) {
            moveTaskToBack(true)
            return
        }
        w.evaluateJavascript("window.magicdabBack ? window.magicdabBack() : false") { closed ->
            if (closed == "true") return@evaluateJavascript
            if (w.canGoBack()) {
                w.goBack()
            } else {
                moveTaskToBack(true)
            }
        }
    }

    @Deprecated("Android 13+ uses the OnBackInvokedCallback of onCreate")
    override fun onBackPressed() {
        back()
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        when (requestCode) {
            REQUEST_FILE -> {
                files?.onReceiveValue(WebChromeClient.FileChooserParams.parseResult(resultCode, data))
                files = null
            }
        }
    }

    // ---------------------------------------------------------------- the stick

    /** A stick plugged in opened the app (USB_DEVICE_ATTACHED in the manifest). */
    private fun usbAttached(intent: Intent?) {
        if (intent?.action == UsbManager.ACTION_USB_DEVICE_ATTACHED) RadioService.instance?.usbAgain()
    }

    /** A stick plugged in while the app is on screen. */
    private val usbReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            // (the server opens it within 2 s; Android may ask again)
            RadioService.instance?.usbAgain()
        }
    }

    private fun registerUsb() {
        if (usbRegistered) return
        usbRegistered = true
        val filter = IntentFilter(UsbManager.ACTION_USB_DEVICE_ATTACHED)
        if (Build.VERSION.SDK_INT >= 33) registerReceiver(usbReceiver, filter, Context.RECEIVER_NOT_EXPORTED) else registerReceiver(usbReceiver, filter)
    }

    private fun unregisterUsb() {
        if (!usbRegistered) return
        usbRegistered = false
        unregisterReceiver(usbReceiver)
    }

    /** Once, when it first plays: the notification with the station (Android 13+). */
    private fun askForNotifications() {
        if (Build.VERSION.SDK_INT < 33) return
        if (checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) return
        if (MagicDab.firstTime(this, "askedNotifications")) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), REQUEST_NOTIFICATIONS)
        }
    }
}
