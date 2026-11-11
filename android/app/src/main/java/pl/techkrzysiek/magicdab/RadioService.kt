package pl.techkrzysiek.magicdab

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.drawable.Icon
import android.media.AudioAttributes
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.media.MediaMetadata
import android.media.session.MediaSession
import android.media.session.PlaybackState
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.PowerManager
import android.util.Base64
import android.util.Log

/**
 * Keeps the receiver running: the server process, and the app's process
 * itself while the page plays with the screen off (a foreground service of
 * the media playback kind, its notification with the station and controls).
 * Without listening and out of sight it stops after a while (battery).
 */
class RadioService : Service() {
    companion object {
        const val ACTION_TOGGLE = "pl.techkrzysiek.magicdab.TOGGLE"
        const val ACTION_QUIT = "pl.techkrzysiek.magicdab.QUIT"
        private const val CHANNEL = "playback"
        private const val NOTIFICATION = 1
        private const val IDLE_STOP_MS = 3 * 60_000L
        private const val SCAN_MS = 5 * 60_000L

        @Volatile
        var instance: RadioService? = null
            private set

        /** Asked for (it starts a moment later): the activity does not ask twice. */
        @Volatile
        var started = false
            private set

        /** From the activity on screen (a foreground service may not start from the background). */
        fun start(context: Context) {
            try {
                context.startForegroundService(Intent(context, RadioService::class.java))
                started = true
            } catch (e: Exception) {
                Log.e(MagicDab.TAG, "cannot start the service: $e")
            }
        }
    }

    private val main = Handler(Looper.getMainLooper())
    private lateinit var session: MediaSession
    private lateinit var server: ServerProcess

    /** The stick for the server (the app opens it, UsbBridge). */
    private lateinit var usb: UsbBridge

    /** The stick plugged in, the app on screen: Android may be asked again. */
    fun usbAgain() {
        if (::usb.isInitialized) usb.again()
    }
    private lateinit var audio: AudioManager
    private var playing = false
    private var title = ""
    private var text = ""
    private var album = ""
    private var art: Bitmap? = null
    private var wake: PowerManager.WakeLock? = null
    private var focus: AudioFocusRequest? = null

    /** Stopped for a call or another app's short sound: playing again after it. */
    private var resumeAfterFocus = false
    private var noisyRegistered = false

    /** The server answers; [status]: why it does not. */
    var ready = false
        private set
    var status: String? = null
        private set

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        instance = this
        audio = getSystemService(AudioManager::class.java)
        title = getString(R.string.app_name)
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.channel_playback), NotificationManager.IMPORTANCE_LOW),
        )
        session = MediaSession(this, "MagicDAB").apply {
            setCallback(object : MediaSession.Callback() {
                override fun onPlay() = MagicDab.pageAction("play")
                override fun onPause() = MagicDab.pageAction("stop")
                override fun onStop() = MagicDab.pageAction("stop")
                override fun onSkipToNext() = MagicDab.pageAction("next")
                override fun onSkipToPrevious() = MagicDab.pageAction("previous")
                override fun onCustomAction(action: String, extras: Bundle?) {
                    if (action == ACTION_QUIT) quit()
                }
            })
            setSessionActivity(openApp())
            isActive = true
        }
        updateSession()
        foreground()
        usb = UsbBridge(this).also { it.start() }
        server = ServerProcess(this, usb.name) { r, s -> main.post { onServer(r, s) } }
        server.start()
        idleCheck()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // (every startForegroundService wants its startForeground)
        foreground()
        when (intent?.action) {
            ACTION_TOGGLE -> MagicDab.pageAction(if (playing) "stop" else "play")
            ACTION_QUIT -> quit()
        }
        return START_NOT_STICKY
    }

    /** Swiped away from the recent apps: the page and its sound are gone, the receiver goes too. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        quit()
    }

    override fun onDestroy() {
        instance = null
        started = false
        main.removeCallbacksAndMessages(null)
        server.stop()
        usb.stop()
        releaseWake()
        unregisterNoisy()
        focus?.let { audio.abandonAudioFocusRequest(it) }
        session.release()
        ready = false
        MagicDab.serverChanged()
        super.onDestroy()
    }

    /** Everything ends: the server, this service and (unless [keepApp]) the activity. */
    fun quit(keepApp: Boolean = false) {
        if (!keepApp) MagicDab.page?.get()?.finishAndRemoveTask()
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    /** From the page: it plays or stopped. */
    fun setPlaying(on: Boolean) {
        if (on == playing) return
        playing = on
        if (on) {
            resumeAfterFocus = false
            if (!requestFocus()) {
                // (a call: the radio waits)
                MagicDab.pageAction("stop")
            }
            acquireWake()
            registerNoisy()
        } else {
            if (!resumeAfterFocus) focus?.let { audio.abandonAudioFocusRequest(it) }
            releaseWake()
            unregisterNoisy()
        }
        updateSession()
        refresh()
        idleCheck()
    }

    /** From the page: the station, its text, the multiplex and the logo (a data: address). */
    fun setMedia(title: String, text: String, album: String, art: String?) {
        this.title = title.ifBlank { getString(R.string.app_name) }
        this.text = text
        this.album = album
        this.art = art?.let { decode(it) }
        updateSession()
        refresh()
    }

    /** The receiver stops when no one listens and the app is out of sight. */
    fun idleCheck() {
        main.removeCallbacks(idleStop)
        if (!playing && !MagicDab.visible) main.postDelayed(idleStop, IDLE_STOP_MS)
    }

    /**
     * A band scan runs (the first one of a new installation, say, with the
     * phone put away): not stopped for want of a listener until it is over, at
     * most SCAN_MS (a page out of sight may not say that it ended).
     */
    private var scanUntil = 0L

    fun setScanning(on: Boolean) {
        scanUntil = if (on) android.os.SystemClock.elapsedRealtime() + SCAN_MS else 0L
        idleCheck()
    }

    private val idleStop: Runnable = Runnable {
        val wait = scanUntil - android.os.SystemClock.elapsedRealtime()
        if (wait > 0) {
            main.postDelayed(idleStop, wait)
            return@Runnable
        }
        if (!playing && !MagicDab.visible) {
            Log.i(MagicDab.TAG, "nobody listening: the receiver stops")
            quit(keepApp = true)
        }
    }

    fun serverTail(): String = server.tail()

    private fun onServer(ready: Boolean, status: String?) {
        this.ready = ready
        this.status = status
        MagicDab.serverChanged()
    }

    // ---------------------------------------------------------------- notification

    private fun foreground() {
        try {
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTIFICATION, notification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
            } else {
                startForeground(NOTIFICATION, notification())
            }
        } catch (e: Exception) {
            // (Android 12+: not from the background; it runs in the foreground already then)
            Log.w(MagicDab.TAG, "foreground: $e")
        }
    }

    private fun refresh() {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION, notification())
    }

    private fun notification(): Notification {
        val toggle = action(if (playing) R.drawable.ic_stop else R.drawable.ic_play, if (playing) R.string.stop else R.string.play, ACTION_TOGGLE)
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_magicdab)
            .setContentTitle(title)
            // what plays, else the multiplex (the receiver's state only before a station)
            .setContentText(text.ifEmpty { album.ifEmpty { getString(R.string.running) } })
            .setSubText(if (text.isEmpty()) null else album.ifEmpty { null })
            .setLargeIcon(art)
            .setContentIntent(openApp())
            .setOngoing(true)
            .setShowWhen(false)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setCategory(Notification.CATEGORY_TRANSPORT)
            .addAction(toggle)
            .addAction(action(R.drawable.ic_quit, R.string.quit, ACTION_QUIT))
            .setStyle(Notification.MediaStyle().setMediaSession(session.sessionToken).setShowActionsInCompactView(0, 1))
            .build()
    }

    private fun action(icon: Int, label: Int, what: String): Notification.Action {
        val intent = Intent(this, RadioService::class.java).setAction(what)
        val pending = PendingIntent.getService(this, what.hashCode(), intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
        return Notification.Action.Builder(Icon.createWithResource(this, icon), getString(label), pending).build()
    }

    private fun openApp(): PendingIntent =
        PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)

    /** The lock screen, Bluetooth, the media controls of Android 13+. */
    private fun updateSession() {
        val actions = PlaybackState.ACTION_PLAY or PlaybackState.ACTION_PAUSE or PlaybackState.ACTION_PLAY_PAUSE or
            PlaybackState.ACTION_STOP or PlaybackState.ACTION_SKIP_TO_NEXT or PlaybackState.ACTION_SKIP_TO_PREVIOUS
        session.setPlaybackState(
            PlaybackState.Builder()
                .setActions(actions)
                .setState(if (playing) PlaybackState.STATE_PLAYING else PlaybackState.STATE_PAUSED, PlaybackState.PLAYBACK_POSITION_UNKNOWN, 1f)
                .addCustomAction(PlaybackState.CustomAction.Builder(ACTION_QUIT, getString(R.string.quit), R.drawable.ic_quit).build())
                .build(),
        )
        // (live radio: no duration, no progress bar)
        val meta = MediaMetadata.Builder()
            .putString(MediaMetadata.METADATA_KEY_TITLE, title)
            .putString(MediaMetadata.METADATA_KEY_ARTIST, text)
            .putString(MediaMetadata.METADATA_KEY_ALBUM, album)
            .putString(MediaMetadata.METADATA_KEY_DISPLAY_TITLE, title)
            .putString(MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE, text.ifEmpty { album })
        // the slide or logo: the background of the lock screen's player
        art?.let {
            meta.putBitmap(MediaMetadata.METADATA_KEY_ART, it)
            meta.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, it)
        }
        session.setMetadata(meta.build())
    }

    private fun decode(dataUrl: String): Bitmap? = try {
        val bytes = Base64.decode(dataUrl.substringAfter(','), Base64.DEFAULT)
        // (SVG logos are not read: the notification shows the app's icon then)
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.let { b ->
            // at most 640 px a side: the metadata goes through a binder, which
            // refuses a megabyte picture
            val scale = 640f / maxOf(b.width, b.height)
            if (scale >= 1f) b else Bitmap.createScaledBitmap(b, (b.width * scale).toInt().coerceAtLeast(1), (b.height * scale).toInt().coerceAtLeast(1), true)
        }
    } catch (e: Exception) {
        null
    }

    // ---------------------------------------------------------------- audio focus, staying awake

    private val focusChange = AudioManager.OnAudioFocusChangeListener { change -> onFocus(change) }

    private fun onFocus(change: Int) {
        when (change) {
            AudioManager.AUDIOFOCUS_LOSS -> {
                resumeAfterFocus = false
                MagicDab.pageAction("stop")
            }
            AudioManager.AUDIOFOCUS_LOSS_TRANSIENT -> {
                if (playing) {
                    resumeAfterFocus = true
                    MagicDab.pageAction("stop")
                }
            }
            AudioManager.AUDIOFOCUS_GAIN -> {
                if (resumeAfterFocus) {
                    resumeAfterFocus = false
                    MagicDab.pageAction("play")
                }
            }
            // (ducking for a short sound is done by Android itself)
        }
    }

    private fun requestFocus(): Boolean {
        val request = focus ?: AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
            .setAudioAttributes(
                AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build(),
            )
            .setOnAudioFocusChangeListener(focusChange, main)
            .build()
            .also { focus = it }
        return audio.requestAudioFocus(request) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED
    }

    /** The demodulator and the decoders need the processor with the screen off. */
    private fun acquireWake() {
        val w = wake ?: getSystemService(PowerManager::class.java)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "MagicDAB:radio")
            .also {
                it.setReferenceCounted(false)
                wake = it
            }
        // (renewed with every start of playback)
        w.acquire(12 * 3600_000L)
    }

    private fun releaseWake() {
        val w = wake
        if (w != null && w.isHeld) w.release()
    }

    /** Headphones pulled out: silence, not the loudspeaker. */
    private val noisy = object : BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            if (playing) MagicDab.pageAction("stop")
        }
    }

    private fun registerNoisy() {
        if (noisyRegistered) return
        noisyRegistered = true
        val filter = IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY)
        if (Build.VERSION.SDK_INT >= 33) registerReceiver(noisy, filter, Context.RECEIVER_NOT_EXPORTED) else registerReceiver(noisy, filter)
    }

    private fun unregisterNoisy() {
        if (!noisyRegistered) return
        noisyRegistered = false
        unregisterReceiver(noisy)
    }
}
