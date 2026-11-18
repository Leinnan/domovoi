package com.crayen.domovoi.ui.thread

import android.content.Context
import android.graphics.Bitmap
import android.graphics.ImageDecoder
import android.net.Uri
import android.util.Base64
import java.io.ByteArrayOutputStream

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
}
