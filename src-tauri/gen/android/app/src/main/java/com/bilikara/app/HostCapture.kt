package com.bilikara.app

import android.app.Activity
import android.content.ClipData
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import android.provider.MediaStore
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.Keep
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.FileProvider
import org.json.JSONObject
import java.io.File

/** System camera/photo picker only. No live camera and no image crosses the WebView bridge. */
internal class HostCapture(private val activity: AppCompatActivity) {
  companion object {
    @JvmStatic @Keep external fun decodeLuma(pixels: ByteArray, width: Int, height: Int): String
  }
  private var pending: ((JSONObject?, String?) -> Unit)? = null
  private var photo: File? = null
  private var generation = 0
  private val picker = activity.registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
    val callback = pending ?: return@registerForActivityResult
    val token = generation
    if (result.resultCode != Activity.RESULT_OK) { finish(callback, JSONObject().put("cancelled", true), null); return@registerForActivityResult }
    val uri = photo?.let { Uri.fromFile(it) } ?: result.data?.data
    if (uri == null || (photo == null && uri.scheme != "content")) { finish(callback, null, "cast_capture_invalid"); return@registerForActivityResult }
    Thread({
      val decoded = try {
        val options = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        activity.contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, options) }
        require(options.outWidth in 1..32768 && options.outHeight in 1..32768)
        var sample = 1
        while (options.outWidth / sample > 2048 || options.outHeight / sample > 2048) sample *= 2
        options.inSampleSize = sample; options.inJustDecodeBounds = false
        val bitmap = activity.contentResolver.openInputStream(uri)?.use { BitmapFactory.decodeStream(it, null, options) } ?: error("image")
        try {
          require(bitmap.width <= 2048 && bitmap.height <= 2048)
          val pixels = IntArray(bitmap.width * bitmap.height)
          bitmap.getPixels(pixels, 0, bitmap.width, 0, 0, bitmap.width, bitmap.height)
          val luma = ByteArray(pixels.size) { i ->
            val p = pixels[i]; (((p shr 16 and 255) * 77 + (p shr 8 and 255) * 150 + (p and 255) * 29) shr 8).toByte()
          }
          JSONObject(decodeLuma(luma, bitmap.width, bitmap.height))
        } finally { bitmap.recycle() }
      } catch (_: LinkageError) { JSONObject().put("error", "cast_capture_unavailable") }
        catch (_: Exception) { JSONObject().put("error", "cast_capture_invalid") }
        catch (_: OutOfMemoryError) { JSONObject().put("error", "cast_capture_invalid") }
      activity.runOnUiThread {
        if (generation == token && pending === callback) {
          finish(callback, decoded, decoded.optString("error").takeIf { it.isNotEmpty() })
        }
      }
    }, "bilikara-qr-decode").start()
  }
  fun start(camera: Boolean, callback: (JSONObject?, String?) -> Unit) {
    if (pending != null) { callback(null, "cast_busy"); return }
    pending = callback; generation++
    try {
      val intent = if (camera) {
        val directory = File(activity.cacheDir, "cast-capture").apply { mkdirs() }
        val file = File.createTempFile("capture-", ".jpg", directory); photo = file
        val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", file)
        Intent(MediaStore.ACTION_IMAGE_CAPTURE).putExtra(MediaStore.EXTRA_OUTPUT, uri)
          .apply { clipData = ClipData.newRawUri("photo", uri) }
          .addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_READ_URI_PERMISSION)
      } else Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("image/*")
      picker.launch(intent)
    } catch (_: Exception) { finish(callback, null, "cast_capture_unavailable") }
  }
  private fun finish(callback: (JSONObject?, String?) -> Unit, data: JSONObject?, error: String?) {
    if (pending !== callback) return
    pending = null; photo?.delete(); photo = null
    callback(data, error)
  }
  fun destroy() {
    generation++
    pending?.let { finish(it, JSONObject().put("cancelled", true), null) }
  }
}
