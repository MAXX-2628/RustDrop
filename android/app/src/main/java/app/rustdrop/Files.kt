package app.rustdrop

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import androidx.documentfile.provider.DocumentFile
import java.io.File
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * A picked or shared file, already open. [path] is the name the receiver sees (`/` inside a
 * folder). The core reads it in place through the descriptor, so nothing is copied first.
 */
class Outgoing(val path: String, val pfd: ParcelFileDescriptor)

/**
 * Android hands out content URIs. Outgoing: open them for the core to read directly.
 * Incoming: publish verified files to Downloads.
 */
object Files {
    private const val MAX_FILES = 1000
    /** Cache folder that older versions copied outgoing files into. */
    fun clearOutgoing(ctx: Context) {
        File(ctx.cacheDir, "outgoing").deleteRecursively()
    }

    fun close(items: List<Outgoing>) = items.forEach { runCatching { it.pfd.close() } }

    suspend fun openUris(ctx: Context, uris: List<Uri>): List<Outgoing> = withContext(Dispatchers.IO) {
        val names = mutableSetOf<String>()
        collecting { list ->
            for (uri in uris) list += open(ctx, uri, uniqueName(names, safeName(displayName(ctx, uri))))
        }
    }

    /** A picked folder (Storage Access Framework tree), keeping its structure. */
    suspend fun openTree(ctx: Context, tree: Uri): List<Outgoing> = withContext(Dispatchers.IO) {
        val root = requireNotNull(DocumentFile.fromTreeUri(ctx, tree)) { "Couldn't open that folder." }
        collecting { list -> walk(ctx, root, safeName(root.name ?: "Folder"), list) }
    }

    private fun walk(ctx: Context, doc: DocumentFile, path: String, list: MutableList<Outgoing>) {
        if (doc.isDirectory) {
            val names = mutableSetOf<String>()
            for (child in doc.listFiles()) {
                walk(ctx, child, "$path/${uniqueName(names, safeName(child.name ?: "file"))}", list)
            }
        } else if (doc.isFile) {
            // Same limit as the core (transfer::MAX_FILES); stop before opening thousands of files.
            require(list.size < MAX_FILES) { "That folder has more than $MAX_FILES files. Choose a smaller folder." }
            list += open(ctx, doc.uri, path)
        }
    }

    /** Closes whatever was opened if a later file fails, so no descriptor leaks. */
    private inline fun collecting(block: (MutableList<Outgoing>) -> Unit): List<Outgoing> {
        val list = mutableListOf<Outgoing>()
        try {
            block(list)
        } catch (e: Throwable) {
            close(list)
            throw e
        }
        return list
    }

    private fun open(ctx: Context, uri: Uri, path: String): Outgoing {
        val pfd = requireNotNull(ctx.contentResolver.openFileDescriptor(uri, "r")) { "Couldn't open a selected file." }
        // statSize is -1 unless this is a regular file (the core reads it at several offsets at once).
        if (pfd.statSize >= 0) return Outgoing(path, pfd)
        pfd.close()
        // A provider that only streams (a pipe): copy this one file to cache and send the copy.
        val tmp = File.createTempFile("outgoing", null, ctx.cacheDir)
        try {
            ctx.contentResolver.openInputStream(uri).use { input ->
                requireNotNull(input) { "Couldn't open a selected file." }
                tmp.outputStream().use { input.copyTo(it) }
            }
            return Outgoing(path, ParcelFileDescriptor.open(tmp, ParcelFileDescriptor.MODE_READ_ONLY))
        } finally {
            tmp.delete() // the open descriptor keeps the data readable until it is closed
        }
    }

    /**
     * Moves verified files from app storage into Downloads/RustDrop, keeping folder structure.
     * MediaStore renames on name clashes, so nothing in Downloads is overwritten.
     * A file is deleted from app storage only after it was fully written to Downloads.
     */
    fun exportToDownloads(ctx: Context, root: File, saved: List<String>): Boolean {
        val resolver = ctx.contentResolver
        var ok = true
        for (path in saved) {
            val file = File(path)
            val parent = file.parentFile?.relativeTo(root)?.invariantSeparatorsPath.orEmpty()
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, file.name)
                put(MediaStore.Downloads.MIME_TYPE, mimeType(file.name))
                put(MediaStore.Downloads.RELATIVE_PATH, listOf("Download/MaxxDrop", parent).filter { it.isNotEmpty() }.joinToString("/"))
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            if (uri == null) {
                ok = false
                continue
            }
            val written = runCatching {
                resolver.openOutputStream(uri).use { out ->
                    requireNotNull(out)
                    file.inputStream().use { it.copyTo(out) }
                }
            }.isSuccess
            if (written) {
                resolver.update(uri, ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }, null, null)
                file.delete()
            } else {
                resolver.delete(uri, null, null)
                ok = false
            }
        }
        return ok
    }

    private fun displayName(ctx: Context, uri: Uri): String =
        ctx.contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        } ?: uri.lastPathSegment ?: "file"

    /** Provider names are untrusted: keep them to one path component. The receiver validates again. */
    private fun safeName(name: String): String =
        name.replace('/', '_').replace('\\', '_').trim().trimEnd('.').ifEmpty { "file" }.take(200)

    /** The receiver rejects names that differ only in case, so `a.jpg` and `A.JPG` become `a.jpg`, `A (1).JPG`. */
    private fun uniqueName(taken: MutableSet<String>, name: String): String {
        var candidate = name
        var n = 1
        while (!taken.add(candidate.lowercase())) {
            val dot = name.lastIndexOf('.').takeIf { it > 0 } ?: name.length
            candidate = "${name.substring(0, dot)} ($n)${name.substring(dot)}"
            n++
        }
        return candidate
    }

    private fun mimeType(name: String): String =
        MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())
            ?: "application/octet-stream"
}
