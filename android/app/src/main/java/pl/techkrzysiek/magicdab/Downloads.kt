package pl.techkrzysiek.magicdab

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore
import android.util.Base64
import android.util.Log
import android.widget.Toast
import java.io.File
import java.io.FileOutputStream
import java.io.OutputStream

/**
 * Files the page saves (exports, recordings): a WebView keeps no downloads of
 * its own, so they come in parts through the bridge into the phone's
 * Downloads/MagicDAB (Android 10+), else the app's own Download folder.
 */
object Downloads {
    private class Open(val name: String, val stream: OutputStream, val uri: Uri?, val file: File?)

    private val open = HashMap<Int, Open>()
    private var next = 1

    /** A new file; its id, 0 when it cannot be made. */
    @Synchronized
    fun start(context: Context, name: String, mime: String): Int {
        val safe = name.replace(Regex("[\\\\/:*?\"<>|\\u0000-\\u001f]"), "_").ifBlank { "magicdab" }
        return try {
            val o = if (Build.VERSION.SDK_INT >= 29) {
                val values = ContentValues().apply {
                    put(MediaStore.MediaColumns.DISPLAY_NAME, safe)
                    put(MediaStore.MediaColumns.MIME_TYPE, mime)
                    put(MediaStore.MediaColumns.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS + "/MagicDAB")
                    put(MediaStore.MediaColumns.IS_PENDING, 1)
                }
                val uri = context.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: return 0
                val stream = context.contentResolver.openOutputStream(uri) ?: return 0
                Open(safe, stream, uri, null)
            } else {
                val dir = context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: return 0
                val file = File(dir, safe)
                Open(safe, FileOutputStream(file), null, file)
            }
            val id = next++
            open[id] = o
            id
        } catch (e: Exception) {
            Log.w(MagicDab.TAG, "cannot save $name: $e")
            0
        }
    }

    @Synchronized
    fun chunk(context: Context, id: Int, base64: String): Boolean {
        val o = open[id] ?: return false
        return try {
            o.stream.write(Base64.decode(base64, Base64.DEFAULT))
            true
        } catch (e: Exception) {
            Log.w(MagicDab.TAG, "cannot save ${o.name}: $e")
            end(context, id, false)
            false
        }
    }

    /** The file is complete ([ok]) and shown in Downloads, or removed. */
    @Synchronized
    fun end(context: Context, id: Int, ok: Boolean) {
        val o = open.remove(id) ?: return
        var saved = ok
        try {
            o.stream.close()
        } catch (e: Exception) {
            saved = false
        }
        try {
            if (o.uri != null) {
                if (saved) {
                    context.contentResolver.update(o.uri, ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }, null, null)
                } else {
                    context.contentResolver.delete(o.uri, null, null)
                }
            } else if (!saved) {
                o.file?.delete()
            }
        } catch (e: Exception) {
            saved = false
        }
        val text = if (!saved) {
            context.getString(R.string.save_failed, o.name)
        } else if (o.file != null) {
            context.getString(R.string.saved, o.file.path)
        } else {
            context.getString(R.string.saved, Environment.DIRECTORY_DOWNLOADS + "/MagicDAB/" + o.name)
        }
        Handler(Looper.getMainLooper()).post { Toast.makeText(context, text, Toast.LENGTH_LONG).show() }
    }
}
