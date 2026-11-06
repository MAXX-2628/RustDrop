package app.rustdrop

import android.Manifest
import android.app.DownloadManager
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.IntentCompat
import androidx.lifecycle.lifecycleScope
import app.rustdrop.ui.Actions
import app.rustdrop.ui.PickKind
import app.rustdrop.ui.RustDropTheme
import app.rustdrop.ui.RustDropUi
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {
    /** Device chosen before the picker opened; null means "stage, then pick a device". */
    private var target: String? = null

    private val pickMedia = registerForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { uris ->
        if (uris.isNotEmpty()) prepare { Files.copyUris(this, uris) }
    }
    private val pickFiles = registerForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        if (uris.isNotEmpty()) prepare { Files.copyUris(this, uris) }
    }
    private val pickFolder = registerForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        if (uri != null) prepare { Files.copyTree(this, uri) }
    }
    private val notifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        askForNotifications()
        handleShare(intent)

        val actions = Actions(
            pick = { kind, device -> pick(kind, device) },
            send = Core::send,
            clearStaged = Core::clearStaged,
            refresh = { if (Core.started) Core.node.refresh() },
            cancel = { id -> Core.node.cancel(id) },
            respond = Core::respond,
            pair = Core::pair,
            unpair = Core::unpair,
            copy = { text -> copy(text) },
            openDownloads = { startActivity(Intent(DownloadManager.ACTION_VIEW_DOWNLOADS)) },
        )
        setContent {
            RustDropTheme {
                RustDropUi(actions)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handleShare(intent)
    }

    private fun pick(kind: PickKind, device: String?) {
        target = device
        when (kind) {
            PickKind.Media -> pickMedia.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo))
            PickKind.Files -> pickFiles.launch(arrayOf("*/*"))
            PickKind.Folder -> pickFolder.launch(null)
        }
    }

    /** Copies picked content off the main thread, then sends it or stages it for a device. */
    private fun prepare(copy: suspend () -> List<String>) {
        val device = target
        target = null
        lifecycleScope.launch {
            Core.preparing.value = true
            val paths = runCatching { copy() }
            Core.preparing.value = false
            paths.onSuccess { if (device != null) Core.send(device, it) else Core.staged.value = it }
                .onFailure { Core.notice.value = it.message ?: "Couldn't read the selected files." }
        }
    }

    private fun handleShare(intent: Intent?) {
        val uris: List<Uri> = when (intent?.action) {
            Intent.ACTION_SEND -> listOfNotNull(IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java))
            Intent.ACTION_SEND_MULTIPLE ->
                IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
            else -> return
        }
        if (uris.isNotEmpty()) {
            target = null
            prepare { Files.copyUris(this, uris) }
        }
    }

    private fun askForNotifications() {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    private fun copy(text: String) {
        getSystemService(ClipboardManager::class.java)
            .setPrimaryClip(ClipData.newPlainText("RustDrop fingerprint", text))
    }
}
