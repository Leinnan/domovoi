package com.crayen.domovoi.ui.thread

import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.os.Environment
import android.provider.MediaStore
import android.util.Base64
import android.webkit.MimeTypeMap
import androidx.core.content.FileProvider
import com.crayen.domovoi.core.Http
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.Request
import java.io.ByteArrayOutputStream
import java.io.File

/** A photo ready to attach: JPEG (not every model reads HEIC), at most 2048 px on its long side. */
data class PreparedImage(val name: String, val sizeBytes: Long, val dataUrl: String)

object Images {
    fun prepare(context: Context, uri: Uri, name: String = "photo.jpg"): PreparedImage? = runCatching {
        val source = ImageDecoder.createSource(context.contentResolver, uri)
        val bitmap = ImageDecoder.decodeBitmap(source) { decoder, info, _ ->
            val longest = maxOf(info.size.width, info.size.height)
            if (longest > 2048) {
                val scale = 2048f / longest
                decoder.setTargetSize((info.size.width * scale).toInt(), (info.size.height * scale).toInt())
            }
            decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
        }
        fromBitmap(bitmap, name)
    }.getOrNull()

    fun fromBitmap(bitmap: Bitmap, name: String = "photo.jpg"): PreparedImage {
        val out = ByteArrayOutputStream()
        bitmap.compress(Bitmap.CompressFormat.JPEG, 85, out)
        val bytes = out.toByteArray()
        return PreparedImage(name, bytes.size.toLong(), "data:image/jpeg;base64," + Base64.encodeToString(bytes, Base64.NO_WRAP))
    }

    private val client by lazy { Http.defaultClient() }

    /** An image the server serves, as it was (its URL carries the signature). */
    private suspend fun download(url: String): Pair<ByteArray, String> = withContext(Dispatchers.IO) {
        client.newCall(Request.Builder().url(url).build()).execute().use { response ->
            check(response.isSuccessful) { "The server answered ${response.code}." }
            response.body.bytes() to (response.header("Content-Type")?.substringBefore(';')?.trim()?.takeIf { it.startsWith("image/") } ?: "image/png")
        }
    }

    /** `name` with an extension for its type (`shot` → `shot.png`). */
    private fun fileName(name: String, mime: String): String {
        val base = name.substringAfterLast('/').ifEmpty { "image" }
        if (base.contains('.')) return base
        return base + "." + (MimeTypeMap.getSingleton().getExtensionFromMimeType(mime) ?: "png")
    }

    /** Saves the image to Pictures/Domovoi; what to tell the user. */
    suspend fun save(context: Context, url: String, name: String): Result<String> = runCatching {
        val (bytes, mime) = download(url)
        val file = fileName(name, mime)
        withContext(Dispatchers.IO) {
            val values = ContentValues().apply {
                put(MediaStore.Images.Media.DISPLAY_NAME, file)
                put(MediaStore.Images.Media.MIME_TYPE, mime)
                put(MediaStore.Images.Media.RELATIVE_PATH, Environment.DIRECTORY_PICTURES + "/Domovoi")
                put(MediaStore.Images.Media.IS_PENDING, 1)
            }
            val resolver = context.contentResolver
            val uri = checkNotNull(resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)) { "The photo library refused the image." }
            try {
                checkNotNull(resolver.openOutputStream(uri)) { "The image couldn't be written." }.use { it.write(bytes) }
                resolver.update(uri, ContentValues().apply { put(MediaStore.Images.Media.IS_PENDING, 0) }, null, null)
            } catch (e: Exception) {
                resolver.delete(uri, null, null)
                throw e
            }
        }
        "Saved $file to Pictures/Domovoi"
    }

    /** Offers the image to other apps (the share sheet). */
    suspend fun share(context: Context, url: String, name: String): Result<Unit> = runCatching {
        val (bytes, mime) = download(url)
        val file = withContext(Dispatchers.IO) {
            File(context.cacheDir, "shared").apply { mkdirs() }.resolve(fileName(name, mime)).apply { writeBytes(bytes) }
        }
        val uri = FileProvider.getUriForFile(context, context.packageName + ".files", file)
        val send = Intent(Intent.ACTION_SEND).setType(mime).putExtra(Intent.EXTRA_STREAM, uri).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        context.startActivity(Intent.createChooser(send, null).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
    }
}
