package io.github.phiresky.wayland_android

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import java.io.File
import java.io.FileOutputStream

/**
 * Transparent Activity that launches Android's native file picker.
 * Started from Rust (portal.rs) when a Linux app opens a file dialog
 * via XDG Desktop Portal. Copies selected files into the Arch rootfs
 * and returns the paths via JNI.
 */
class FileChooserActivity : Activity() {

    private var requestId = ""
    private var requestType = "open_file"

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        requestId = intent.getStringExtra("request_id") ?: ""
        requestType = intent.getStringExtra("request_type") ?: "open_file"
        val title = intent.getStringExtra("title") ?: "Choose a file"
        val multiple = intent.getBooleanExtra("multiple", false)
        val directory = intent.getBooleanExtra("directory", false)
        val mimeTypesStr = intent.getStringExtra("mime_types") ?: "*/*"
        val currentName = intent.getStringExtra("current_name") ?: ""

        if (requestId.isEmpty()) {
            sendResult(2, "")
            finish()
            return
        }

        val pickerIntent = when (requestType) {
            "save_file" -> {
                Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
                    addCategory(Intent.CATEGORY_OPENABLE)
                    type = parseMimeTypes(mimeTypesStr).firstOrNull() ?: "*/*"
                    if (currentName.isNotEmpty()) {
                        putExtra(Intent.EXTRA_TITLE, currentName)
                    }
                }
            }
            else -> {
                if (directory) {
                    Intent(Intent.ACTION_OPEN_DOCUMENT_TREE)
                } else {
                    Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
                        addCategory(Intent.CATEGORY_OPENABLE)
                        val mimeTypes = parseMimeTypes(mimeTypesStr)
                        if (mimeTypes.size <= 1) {
                            type = mimeTypes.firstOrNull() ?: "*/*"
                        } else {
                            type = "*/*"
                            putExtra(Intent.EXTRA_MIME_TYPES, mimeTypes.toTypedArray())
                        }
                        putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple)
                    }
                }
            }
        }

        try {
            startActivityForResult(Intent.createChooser(pickerIntent, title), REQUEST_CODE)
        } catch (e: Exception) {
            sendResult(2, "")
            finish()
        }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)

        if (requestCode != REQUEST_CODE) {
            sendResult(2, "")
            finish()
            return
        }

        if (resultCode != RESULT_OK || data == null) {
            sendResult(1, "") // User cancelled
            finish()
            return
        }

        // Collect all selected URIs
        val uris = mutableListOf<Uri>()
        val clipData = data.clipData
        if (clipData != null) {
            for (i in 0 until clipData.itemCount) {
                clipData.getItemAt(i).uri?.let { uris.add(it) }
            }
        } else {
            data.data?.let { uris.add(it) }
        }

        if (uris.isEmpty()) {
            sendResult(1, "")
            finish()
            return
        }

        // Copy files to rootfs in a background thread
        val rootfs = applicationInfo.dataDir + "/files/arch"
        Thread {
            val paths = mutableListOf<String>()
            for (uri in uris) {
                val path = if (requestType == "save_file") {
                    // For save: return a temp path. The app writes to it,
                    // then we need to copy back. For now, just map the URI.
                    copyUriToRootfs(uri, rootfs)
                } else {
                    copyUriToRootfs(uri, rootfs)
                }
                if (path != null) {
                    paths.add(path)
                }
            }

            if (paths.isEmpty()) {
                sendResult(2, "")
            } else {
                sendResult(0, paths.joinToString("\n"))
            }
            runOnUiThread { finish() }
        }.start()
    }

    /**
     * Copy a content URI to a file inside the rootfs tmp directory.
     * Returns the proot-visible path (e.g. /tmp/portal-files/abc/document.txt).
     */
    private fun copyUriToRootfs(uri: Uri, rootfs: String): String? {
        return try {
            val fileName = getFileName(uri) ?: "file_${System.currentTimeMillis()}"
            val subdir = "portal-${requestId}"
            val destDir = File("$rootfs/tmp/portal-files/$subdir")
            destDir.mkdirs()
            val destFile = File(destDir, fileName)

            contentResolver.openInputStream(uri)?.use { input ->
                FileOutputStream(destFile).use { output ->
                    input.copyTo(output)
                }
            }

            // Return the path as seen from inside proot
            "/tmp/portal-files/$subdir/$fileName"
        } catch (e: Exception) {
            null
        }
    }

    private fun getFileName(uri: Uri): String? {
        // Try to get display name from content resolver
        try {
            contentResolver.query(uri, null, null, null, null)?.use { cursor ->
                val nameIndex = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (nameIndex >= 0 && cursor.moveToFirst()) {
                    return cursor.getString(nameIndex)
                }
            }
        } catch (_: Exception) {}

        // Fall back to last path segment
        return uri.lastPathSegment?.substringAfterLast('/')
    }

    private fun sendResult(responseCode: Int, paths: String) {
        nativeFileChooserResult(requestId, responseCode, paths)
    }

    private fun parseMimeTypes(str: String): List<String> {
        if (str.isBlank()) return listOf("*/*")
        return str.split(",").map { it.trim() }.filter { it.isNotEmpty() }
    }

    companion object {
        private const val REQUEST_CODE = 42

        init {
            System.loadLibrary("android_wayland_launcher")
        }

        @JvmStatic
        private external fun nativeFileChooserResult(requestId: String, responseCode: Int, paths: String)
    }
}
