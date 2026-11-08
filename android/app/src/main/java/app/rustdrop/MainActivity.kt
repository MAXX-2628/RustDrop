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
import androidx.core.net.toUri
import androidx.lifecycle.lifecycleScope
import app.rustdrop.ui.Actions
import app.rustdrop.ui.PickKind
import app.rustdrop.ui.RustDropTheme
import app.rustdrop.ui.RustDropUi
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {
    /** Device chosen before the picker opened; null means "stage, then pick a device". */
    private var target: String? = null

    private val pickMedia = registerForActivityResult(ActivityResultContracts.PickMultipleVisualMedia()) { uris ->
        if (uris.isNotEmpty()) prepare { Files.openUris(this, uris) }
    }
    private val pickFiles = registerForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { uris ->
        if (uris.isNotEmpty()) prepare { Files.openUris(this, uris) }
    }
    private val pickFolder = registerForActivityResult(ActivityResultContracts.OpenDocumentTree()) { uri ->
        if (uri != null) prepare { Files.openTree(this, uri) }
    }
    private val notifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}
    private val scanQr = registerForActivityResult(ScanContract()) { result ->
        result.contents?.let { link -> lifecycleScope.launch { Core.pairWithLink(link) } }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        askForNotifications()
        handleShare(intent)

        val actions = Actions(
            pick = { kind, device -> pick(kind, device) },
            send = Core::send,
            sendAll = Core::sendAll,
            stageText = Core::stageText,
            sendText = { devices, text -> lifecycleScope.launch { Core.sendText(devices, text) } },
            dismissText = Core::dismissText,
            openLink = { url ->
                if (isLink(url)) runCatching { startActivity(Intent(Intent.ACTION_VIEW, url.toUri())) }
            },
            copyText = { text -> copy(text, "Ferrydrop") },
            pause = Core::pause,
            resume = Core::resume,
            clearHistory = Core::clearHistory,
            rename = Core::rename,
            setAutoAccept = Core::setAutoAccept,
            clearStaged = Core::clearStaged,
            refresh = { if (Core.started) Core.node.refresh() },
            cancel = Core::cancel,
            respond = Core::respond,
            pair = Core::pair,
            unpair = Core::unpair,
            copy = { text -> copy(text, "Ferrydrop fingerprint") },
            openDownloads = { startActivity(Intent(DownloadManager.ACTION_VIEW_DOWNLOADS)) },
            scanQr = {
                scanQr.launch(
                    ScanOptions()
                        .setDesiredBarcodeFormats(ScanOptions.QR_CODE)
                        .setPrompt("Scan the QR code shown on the other device")
                        .setBeepEnabled(false)
                        .setOrientationLocked(false),
                )
            },
            showQr = Core::startPairing,
            hideQr = Core::stopPairing,
            setClipboardSync = Core::setClipboardSync,
        )
        setContent {
            RustDropTheme {
                RustDropUi(actions)
            }
        }
    }

    // Android lets only the app on screen read the clipboard, so sync reads it on focus and on change.
    private val clipboard by lazy { getSystemService(ClipboardManager::class.java) }
    private val clipListener = ClipboardManager.OnPrimaryClipChangedListener { shareClipboard() }
    private var lastClipStamp = 0L

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) shareClipboard()
    }

    private fun shareClipboard() {
        if (!Core.clipboardSync.value) return
        val clip = clipboard.primaryClip ?: return
        // Skip a clip already read, so focusing the app doesn't re-read (and re-toast) the same copy.
        val stamp = clip.description.timestamp
        if (stamp == lastClipStamp || clip.itemCount == 0) return
        lastClipStamp = stamp
        val item = clip.getItemAt(0)
        val uri = item.uri
        if (uri != null && clip.description.hasMimeType("image/*")) {
            // Our own provider serves the image that just arrived: never send it back.
            if (uri.authority != Core.clipAuthority) lifecycleScope.launch { Core.clipboardImage(uri) }
            return
        }
        val text = item.coerceToText(this)?.toString() ?: return
        Core.clipboardChanged(text)
    }

    // Discovery and the multicast lock only while visible (battery); see Core.setVisible.
    override fun onStart() {
        super.onStart()
        Core.setVisible(true)
        clipboard.addPrimaryClipChangedListener(clipListener)
    }

    override fun onStop() {
        clipboard.removePrimaryClipChangedListener(clipListener)
        Core.setVisible(false)
        super.onStop()
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

    /** Opens picked content off the main thread, then sends it or stages it for a device. */
    private fun prepare(open: suspend () -> List<Outgoing>) {
        val device = target
        target = null
        lifecycleScope.launch {
            Core.preparing.value = true
            val items = runCatching { open() }
            // From Direct Share the app may just have started: give discovery a moment to list the device.
            if (device != null) awaitPaired(device)
            Core.preparing.value = false
            items.onSuccess { if (device != null) Core.send(device, it) else Core.stage(it) }
                .onFailure { Core.notice.value = it.message ?: "Couldn't read the selected files." }
        }
    }

    private fun handleShare(intent: Intent?) {
        if (intent?.action == ACTION_PICK_FILES) {
            pick(PickKind.Files, null) // from the Quick Settings tile
            return
        }
        val uris: List<Uri> = when (intent?.action) {
            Intent.ACTION_SEND -> listOfNotNull(IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java))
            Intent.ACTION_SEND_MULTIPLE ->
                IntentCompat.getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java).orEmpty()
            else -> return
        }
        // Direct Share: the user picked a paired device in the share sheet.
        val device = intent.getStringExtra(androidx.core.content.pm.ShortcutManagerCompat.EXTRA_SHORTCUT_ID)
        if (uris.isNotEmpty()) {
            target = device
            prepare { Files.openUris(this, uris) }
            return
        }
        // A shared link or text (a browser's Share, for example).
        val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()?.trim()
        if (!text.isNullOrEmpty()) {
            if (device == null) return Core.stageText(text)
            lifecycleScope.launch {
                if (awaitPaired(device)) Core.sendText(listOf(device), text) else Core.stageText(text)
            }
        }
    }

    /** Waits up to 5 s for a paired device to be listed. */
    private suspend fun awaitPaired(device: String) = kotlinx.coroutines.withTimeoutOrNull(5_000) {
        Core.peers.first { list -> list.any { it.id == device && it.trusted == true } }
    } != null

    private fun askForNotifications() {
        if (Build.VERSION.SDK_INT >= 33 &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    private fun copy(text: String, label: String) {
        getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText(label, text))
    }

    companion object {
        const val ACTION_PICK_FILES = "app.rustdrop.action.PICK_FILES"
    }
}
