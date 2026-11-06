package app.rustdrop

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.provider.MediaStore
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import androidx.documentfile.provider.DocumentFile
import java.io.File
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * The core works with file paths; Android hands out content URIs.
 * Outgoing: copy picked/shared content into cache. Incoming: publish verified files to Downloads.
 */
object Files {
    private fun outgoingRoot(ctx: Context) = File(ctx.cacheDir, "outgoing")

    fun clearOutgoing(ctx: Context) {
        outgoingRoot(ctx).deleteRecursively()
    }

    // shortcut: copies picked files into cache before sending (needs free space equal to their size);
    // stream straight from the ContentResolver if large videos become a problem.
    suspend fun copyUris(ctx: Context, uris: List<Uri>): List<String> = withContext(Dispatchers.IO) {
        val dir = File(outgoingRoot(ctx), UUID.randomUUID().toString()).apply { mkdirs() }
        uris.map { uri ->
            val target = unique(dir, safeName(displayName(ctx, uri)))
            ctx.contentResolver.openInputStream(uri).use { input ->
                requireNotNull(input) { "Couldn't open a selected file." }
                target.outputStream().use { input.copyTo(it) }
            }
            target.path
        }
    }

    /** Copies a picked folder (Storage Access Framework tree), keeping its structure. */
    suspend fun copyTree(ctx: Context, tree: Uri): List<String> = withContext(Dispatchers.IO) {
        val root = requireNotNull(DocumentFile.fromTreeUri(ctx, tree)) { "Couldn't open that folder." }
        val dir = File(outgoingRoot(ctx), UUID.randomUUID().toString())
        val top = File(dir, safeName(root.name ?: "Folder"))
        copyDocument(ctx, root, top)
        listOf(top.path)
    }

    private fun copyDocument(ctx: Context, doc: DocumentFile, target: File) {
        if (doc.isDirectory) {
            target.mkdirs()
            for (child in doc.listFiles()) {
                copyDocument(ctx, child, unique(target, safeName(child.name ?: "file")))
            }
        } else if (doc.isFile) {
            ctx.contentResolver.openInputStream(doc.uri)?.use { input ->
                target.outputStream().use { input.copyTo(it) }
            }
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
                put(MediaStore.Downloads.RELATIVE_PATH, listOf("Download/RustDrop", parent).filter { it.isNotEmpty() }.joinToString("/"))
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

    private fun unique(dir: File, name: String): File {
        var candidate = File(dir, name)
        var n = 1
        while (candidate.exists()) {
            val dot = name.lastIndexOf('.').takeIf { it > 0 } ?: name.length
            candidate = File(dir, "${name.substring(0, dot)} ($n)${name.substring(dot)}")
            n++
        }
        return candidate
    }

    private fun mimeType(name: String): String =
        MimeTypeMap.getSingleton().getMimeTypeFromExtension(name.substringAfterLast('.', "").lowercase())
            ?: "application/octet-stream"
}
