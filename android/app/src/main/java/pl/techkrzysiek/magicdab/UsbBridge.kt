package pl.techkrzysiek.magicdab

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.hardware.usb.UsbDevice
import android.hardware.usb.UsbDeviceConnection
import android.hardware.usb.UsbManager
import android.net.LocalServerSocket
import android.net.LocalSocket
import android.os.Build
import android.os.ParcelFileDescriptor
import android.os.Process
import android.util.Log
import java.io.IOException
import kotlin.concurrent.thread

/**
 * The stick for the server without a driver app: an app may not open USB
 * devices itself, Android opens them for it (with the user's permission), so
 * the app asks Android and hands the open device's file descriptor to the
 * server over a local socket (crates/sdr/src/android_usb.rs). Only the
 * server, a process of this app, gets it (the socket checks the uid).
 */
class UsbBridge(private val context: Context) {
    companion object {
        private const val ACTION = "pl.techkrzysiek.magicdab.USB_PERMISSION"
    }

    /** The socket's name (abstract namespace), MAGICDAB_USB_SOCKET of the server. */
    val name = "magicdab-usb-" + MagicDab.token
    private val usb = context.getSystemService(UsbManager::class.java)
    private var server: LocalServerSocket? = null

    @Volatile
    private var running = false

    /** The device open for the server (closed when it asks again). */
    private var connection: UsbDeviceConnection? = null

    fun start() {
        if (running) return
        server = try {
            LocalServerSocket(name)
        } catch (e: IOException) {
            Log.e(MagicDab.TAG, "USB bridge: $e")
            return
        }
        running = true
        thread(name = "magicdab-usb") {
            while (running) {
                val s = try {
                    server?.accept() ?: break
                } catch (e: IOException) {
                    break
                }
                thread(name = "magicdab-usb-open") { serve(s) }
            }
        }
    }

    fun stop() {
        running = false
        try {
            server?.close()
        } catch (e: IOException) {
        }
        server = null
        synchronized(this) {
            connection?.close()
            connection = null
        }
    }

    private fun serve(s: LocalSocket) {
        s.use {
            if (s.peerCredentials.uid != Process.myUid()) return
            val line = s.inputStream.bufferedReader().readLine() ?: return
            if (line.trim() == "list") {
                // the sticks plugged in, a line each (for the settings; no permission needed)
                val names = Sticks.all(context).joinToString("") { d ->
                    listOfNotNull(d.manufacturerName, d.productName).joinToString(" ").ifEmpty { "RTL-SDR" } + "\n"
                }
                s.outputStream.write(names.toByteArray())
                s.outputStream.flush()
                return
            }
            if (line.trim() != "open") return
            val reply = try {
                val fd = open()
                s.setFileDescriptorsForSend(arrayOf(fd.fileDescriptor))
                s.outputStream.write("ok\n".toByteArray())
                s.outputStream.flush()
                fd.close()
                return
            } catch (e: Exception) {
                Log.w(MagicDab.TAG, "USB: ${e.message}")
                "error: ${e.message}\n"
            }
            try {
                s.outputStream.write(reply.toByteArray())
            } catch (e: IOException) {
            }
        }
    }

    /** Android's question is on screen (asked once until answered). */
    @Volatile
    private var asking = false

    /**
     * The user said no: not asked again (the server tries every 2 s) until
     * the stick is plugged in again or the app comes on screen again (again()).
     */
    @Volatile
    private var denied = false

    /** When Android was last asked (a question never answered, e.g. under the lock screen, is asked again). */
    @Volatile
    private var askedAt = 0L

    /** The stick plugged in again, the app on screen again: the question may come again. */
    fun again() {
        denied = false
        asking = false
    }

    /**
     * The stick open (a copy of its descriptor for the server). Without the
     * permission yet: Android asks the user and this answers at once that it
     * waits (the server tries again every few seconds and never stops
     * measuring meanwhile).
     */
    @Synchronized
    private fun open(): ParcelFileDescriptor {
        val dev = Sticks.first(context) ?: throw IOException(context.getString(R.string.plug_stick))
        if (!usb.hasPermission(dev)) {
            if (denied) throw IOException(context.getString(R.string.usb_denied))
            // (Android's question only over the app: not out of the blue from the background)
            if (asking && android.os.SystemClock.elapsedRealtime() - askedAt > 60_000) asking = false
            if (!asking && MagicDab.visible) ask(dev)
            throw IOException(context.getString(R.string.usb_waiting))
        }
        connection?.close()
        val c = usb.openDevice(dev) ?: throw IOException(context.getString(R.string.usb_cannot_open))
        connection = c
        return ParcelFileDescriptor.fromFd(c.fileDescriptor)
    }

    private fun ask(dev: UsbDevice) {
        asking = true
        askedAt = android.os.SystemClock.elapsedRealtime()
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(c: Context, i: Intent) {
                asking = false
                if (!i.getBooleanExtra(UsbManager.EXTRA_PERMISSION_GRANTED, false)) denied = true
                try {
                    context.unregisterReceiver(this)
                } catch (e: IllegalArgumentException) {
                }
            }
        }
        val filter = IntentFilter(ACTION)
        if (Build.VERSION.SDK_INT >= 33) context.registerReceiver(receiver, filter, Context.RECEIVER_NOT_EXPORTED) else context.registerReceiver(receiver, filter)
        // (mutable: Android adds the device and the answer to it)
        val flags = if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0
        val pending = PendingIntent.getBroadcast(context, 0, Intent(ACTION).setPackage(context.packageName), flags)
        usb.requestPermission(dev, pending)
    }
}
