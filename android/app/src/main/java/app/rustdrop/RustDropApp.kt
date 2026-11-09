package app.rustdrop

import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import app.rustdrop.core.FdFile
import app.rustdrop.core.HistoryEntry
import app.rustdrop.core.IncomingRequest
import app.rustdrop.core.Listener
import app.rustdrop.core.Me
import app.rustdrop.core.Node
import app.rustdrop.core.NodeException
import app.rustdrop.core.PairingOffer
import app.rustdrop.core.PeerInfo
import app.rustdrop.core.TransferState
import app.rustdrop.core.TransferUpdate
import app.rustdrop.core.TrustedInfo
import java.io.File
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/** Transfer as the UI shows it; built by merging core [TransferUpdate]s. */
data class Transfer(
    val id: String,
    val outgoing: Boolean = false,
    val peer: String = "",
    /** First file or folder name. */
    val name: String = "",
    val items: Long = 0,
    val total: Long = 0,
    val done: Long = 0,
    val state: TransferState = TransferState.WAITING_FOR_ACCEPT,
    val reason: String? = null,
    val savedCount: Int = 0,
    val startedAt: Long = System.currentTimeMillis(),
    /** When bytes started moving; used for speed and time left. */
    val flowingAt: Long? = null,
)

val ACTIVE = setOf(TransferState.WAITING_FOR_ACCEPT, TransferState.TRANSFERRING, TransferState.VERIFYING)

/** A link or text another device sent on purpose. */
data class ReceivedText(val text: String, val from: String, val at: Long = System.currentTimeMillis())

private fun HistoryEntry.toTransfer(): Transfer {
    // The files of a paused send are only held while the app runs.
    val paused = state == TransferState.PAUSED
    return Transfer(
        id = id,
        outgoing = outgoing,
        peer = peer,
        name = name,
        items = items.toLong(),
        total = total.toLong(),
        done = if (state == TransferState.COMPLETED) total.toLong() else 0,
        state = if (paused) TransferState.CANCELLED else state,
        reason = if (paused) "Paused, then MaxxDrop closed. Send the files again to continue." else reason,
        savedCount = saved.size,
        startedAt = finished.toLong() * 1000,
    )
}

private const val REQUEST_TIMEOUT_MS = 60_000L // matches the core's DECISION_TIMEOUT
private const val BACKGROUND_PAUSE_MS = 30_000L

/** Process-wide state: the Rust node and the flows the UI observes. */
object Core {
    lateinit var node: Node
        private set
    private lateinit var app: Application
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    val me = MutableStateFlow<Me?>(null)
    val peers = MutableStateFlow<List<PeerInfo>>(emptyList())
    val trusted = MutableStateFlow<List<TrustedInfo>>(emptyList())
    val transfers = MutableStateFlow<List<Transfer>>(emptyList())
    val requests = MutableStateFlow<List<IncomingRequest>>(emptyList())
    val fatal = MutableStateFlow<String?>(null)
    /** Files opened from pickers or the share sheet, waiting for a device. */
    val staged = MutableStateFlow<List<Outgoing>>(emptyList())
    val preparing = MutableStateFlow(false)
    val notice = MutableStateFlow<String?>(null)

    /** QR pairing: the code this phone is showing, and the outcome to report. */
    val pairingOffer = MutableStateFlow<PairingOffer?>(null)
    val pairingMessage = MutableStateFlow<String?>(null)

    /** Clipboard sync (opt-in), and the device the last clipboard text came from (shown for 3 s). */
    val clipboardSync = MutableStateFlow(false)
    val clipboardFrom = MutableStateFlow<String?>(null)

    /** A link or text waiting for a device (from the share sheet or typed), and texts received. */
    val stagedText = MutableStateFlow<String?>(null)
    val texts = MutableStateFlow<List<ReceivedText>>(emptyList())

    private val received get() = File(app.filesDir, "received")
    private var multicast: WifiManager.MulticastLock? = null
    private val main = Handler(Looper.getMainLooper())

    fun start(application: Application) {
        app = application
        // Android drops incoming multicast (mDNS) packets unless an app holds this lock.
        multicast = app.getSystemService(WifiManager::class.java)
            ?.createMulticastLock("rustdrop-discovery")
            ?.apply { setReferenceCounted(false) }
        Files.clearOutgoing(app)
        Notifications.createChannels(app)
        val name = Settings.Global.getString(app.contentResolver, Settings.Global.DEVICE_NAME)
            ?: android.os.Build.MODEL
        try {
            node = Node(File(app.filesDir, "rustdrop").path, received.path, name, Events)
        } catch (e: NodeException) {
            fatal.value = e.message
            return
        }
        me.value = node.me()
        trusted.value = node.trusted()
        clipboardSync.value = node.clipboardSync()
        transfers.value = node.history().map { it.toTransfer() }
        watchForegroundService()
    }

    val started get() = ::node.isInitialized

    /**
     * Discovery and the multicast lock run only while RustDrop is on screen: the lock wakes the
     * Wi-Fi radio for every multicast packet on the network. Transfers keep running over TCP.
     */
    fun setVisible(visible: Boolean) {
        this.visible = visible
        backgroundPause?.cancel()
        if (visible) {
            multicast?.acquire()
            if (started) node.setDiscovery(true)
            return
        }
        // Wait before pausing: pickers and the QR scanner also hide this screen briefly,
        // and a send right after picking needs the device list.
        backgroundPause = scope.launch {
            delay(BACKGROUND_PAUSE_MS)
            if (started) node.setDiscovery(false)
            multicast?.let { if (it.isHeld) it.release() }
            stopPairing()
        }
    }

    private var backgroundPause: Job? = null

    @Volatile
    private var visible = false

    fun startPairing() {
        pairingMessage.value = null
        pairingOffer.value = try {
            node.startPairing()
        } catch (e: NodeException) {
            pairingMessage.value = e.message
            null
        }
    }

    fun stopPairing() {
        if (pairingOffer.value != null) {
            pairingOffer.value = null
            if (started) node.stopPairing()
        }
    }

    /** Pairs with the device whose QR code was scanned. Blocks, so it runs on the IO dispatcher. */
    suspend fun pairWithLink(link: String) = withContext(Dispatchers.IO) {
        pairingMessage.value = try {
            val name = node.pairWithLink(link)
            trusted.value = node.trusted()
            "Paired with $name."
        } catch (e: NodeException) {
            e.message
        }
    }

    /**
     * Sends opened files. The core gets duplicates of the descriptors and closes them when the
     * transfer ends; ours are closed on success, or kept staged so the user can pick a device again.
     */
    fun send(device: String, items: List<Outgoing>) {
        try {
            node.sendFds(device, items.map { FdFile(it.path, it.pfd.dup().detachFd()) })
            if (staged.value === items) clearStaged() else Files.close(items)
            notice.value = null
        } catch (e: Exception) {
            if (e !is NodeException && e !is java.io.IOException) throw e
            notice.value = e.message ?: "Couldn't send the files."
            if (staged.value !== items) stage(items)
        }
    }

    /** Sends the same files to several devices; each gets its own duplicates of the descriptors. */
    fun sendAll(devices: List<String>, items: List<Outgoing>) {
        val failed = devices.mapNotNull { device ->
            runCatching { node.sendFds(device, items.map { FdFile(it.path, it.pfd.dup().detachFd()) }) }
                .exceptionOrNull()?.let { it.message ?: "Couldn't send." }
        }
        if (staged.value === items) clearStaged() else Files.close(items)
        notice.value = failed.distinct().joinToString(" ").ifEmpty { null }
    }

    /** Holds picked files until a device is chosen, closing any that were staged before. */
    fun stage(items: List<Outgoing>) {
        Files.close(staged.value)
        staged.value = items
        if (items.isNotEmpty()) stagedText.value = null
    }

    fun clearStaged() {
        stage(emptyList())
        stagedText.value = null
    }

    fun stageText(text: String) {
        stage(emptyList())
        stagedText.value = text.trim().takeIf { it.isNotEmpty() }
    }

    /** Sends a link or text to each device; blocks on the network, so it runs on the IO dispatcher. */
    suspend fun sendText(devices: List<String>, text: String) = withContext(Dispatchers.IO) {
        val failed = devices.mapNotNull { runCatching { node.sendText(it, text) }.exceptionOrNull()?.message }
        stagedText.value = null
        notice.value = failed.distinct().joinToString(" ").ifEmpty { null }
    }

    fun dismissText(t: ReceivedText) = texts.update { list -> list.filter { it !== t } }

    fun pause(id: String) {
        if (started) node.pause(id)
    }

    fun resume(id: String) {
        runCatching { node.resume(id) }.onFailure { notice.value = it.message }
    }

    fun clearHistory() {
        runCatching { node.clearHistory() }
            .onSuccess { transfers.update { list -> list.filter { it.state in ACTIVE || it.state == TransferState.PAUSED } } }
            .onFailure { notice.value = it.message }
    }

    /** Returns an error message, or null on success. */
    fun rename(fingerprint: String, name: String): String? = runCatching { node.renameDevice(fingerprint, name) }
        .also { trusted.value = node.trusted() }
        .exceptionOrNull()?.message

    fun setAutoAccept(fingerprint: String, on: Boolean) {
        runCatching { node.setAutoAccept(fingerprint, on) }.onFailure { notice.value = it.message }
        trusted.value = node.trusted()
    }

    /**
     * An image was copied on this phone: send it as PNG to paired devices (clipboard sync).
     * Other formats are converted, since PCs only take PNG. The copy is unlinked once opened.
     */
    suspend fun clipboardImage(uri: android.net.Uri) = withContext(Dispatchers.IO) {
        if (!started || !clipboardSync.value) return@withContext
        val file = File.createTempFile("clip", ".png", app.cacheDir)
        try {
            val resolver = app.contentResolver
            val png = resolver.getType(uri) == "image/png"
            resolver.openInputStream(uri)?.use { input ->
                file.outputStream().use { out ->
                    if (png) {
                        input.copyTo(out)
                    } else {
                        android.graphics.BitmapFactory.decodeStream(input)
                            ?.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, out)
                    }
                }
            } ?: return@withContext
            if (file.length() == 0L) return@withContext
            val pfd = android.os.ParcelFileDescriptor.open(file, android.os.ParcelFileDescriptor.MODE_READ_ONLY)
            runCatching { node.clipboardImageFd(pfd.detachFd()) }
        } catch (e: Exception) {
            if (e is kotlinx.coroutines.CancellationException) throw e
            // Unreadable or not an image: clipboard sync skips it, like text over 64 KB.
        } finally {
            file.delete()
        }
    }

    /** Authority of our FileProvider, which serves received clipboard images. */
    val clipAuthority get() = "${app.packageName}.files"

    fun pair(fingerprint: String, name: String): String? = try {
        node.pair(fingerprint, name)
        trusted.value = node.trusted()
        null
    } catch (e: NodeException) {
        e.message
    }

    fun unpair(fingerprint: String) {
        runCatching { node.unpair(fingerprint) }
        trusted.value = node.trusted()
    }

    fun respond(request: IncomingRequest, accept: Boolean) {
        requests.update { q -> q.filter { it.id != request.id } }
        Notifications.dismissRequest(app, request.id)
        node.respond(request.id, accept)
    }

    /** Decline from the notification, without opening the app. */
    fun decline(id: String) {
        requests.update { q -> q.filter { it.id != id } }
        Notifications.dismissRequest(app, id)
        node.respond(id, false)
    }

    fun setClipboardSync(on: Boolean) {
        if (!started) return
        runCatching { node.setClipboardSync(on) }
            .onSuccess { clipboardSync.value = on }
            .onFailure { notice.value = it.message }
    }

    /** This phone's clipboard changed (read while RustDrop is on screen, as Android requires). */
    fun clipboardChanged(text: String) {
        if (started && clipboardSync.value) node.clipboardChanged(text)
    }

    fun cancel(id: String) {
        if (started) node.cancel(id)
    }

    private fun merge(u: TransferUpdate) = transfers.update { list ->
        val now = System.currentTimeMillis()
        val i = list.indexOfFirst { it.id == u.id }
        val old = if (i >= 0) list[i] else Transfer(u.id, startedAt = now)
        var t = old.copy(
            outgoing = u.outgoing ?: old.outgoing,
            peer = u.peer ?: old.peer,
            name = u.name ?: old.name,
            items = u.items?.toLong() ?: old.items,
            total = u.total?.toLong() ?: old.total,
            done = u.done?.toLong() ?: old.done,
            state = u.state ?: old.state,
            reason = if (u.state != null) u.reason else old.reason,
            savedCount = u.saved?.size ?: old.savedCount,
        )
        if (t.state == TransferState.TRANSFERRING && t.flowingAt == null) t = t.copy(flowingAt = now)
        if (i >= 0) list.toMutableList().also { it[i] = t } else (listOf(t) + list).take(200)
    }

    private fun update(id: String, state: TransferState, reason: String?) =
        TransferUpdate(id, null, null, null, null, null, null, state, reason, null)

    /** Receives core events on Rust threads; flows are thread-safe. */
    private object Events : Listener {
        override fun peersChanged(peers: List<PeerInfo>) {
            Core.peers.value = peers
            Shortcuts.update(app, peers.filter { it.trusted == true })
        }

        override fun textReceived(text: String, from: String) {
            texts.update { (listOf(ReceivedText(text, from)) + it).take(5) }
            if (!visible) Notifications.text(app, text, from)
        }

        override fun clipboardImageReceived(path: String, from: String) {
            // Served to other apps by our FileProvider; one file, replaced by the next image.
            val src = File(path)
            val dir = File(app.cacheDir, "clipboard").apply { mkdirs() }
            val dest = File(dir, "clipboard.png")
            val copied = runCatching { src.copyTo(dest, overwrite = true) }.isSuccess
            src.delete()
            if (!copied) return
            val uri = androidx.core.content.FileProvider.getUriForFile(app, clipAuthority, dest)
            main.post {
                app.getSystemService(ClipboardManager::class.java)
                    .setPrimaryClip(ClipData.newUri(app.contentResolver, "MaxxDrop image", uri))
            }
            clipboardFrom.value = from
            scope.launch {
                delay(3000)
                if (clipboardFrom.value == from) clipboardFrom.value = null
            }
        }

        override fun incoming(request: IncomingRequest) {
            requests.update { it + request }
            if (!visible) Notifications.incoming(app, request)
            scope.launch {
                delay(REQUEST_TIMEOUT_MS)
                if (requests.value.any { it.id == request.id }) {
                    requests.update { q -> q.filter { it.id != request.id } }
                    merge(update(request.id, TransferState.CANCELLED, "Not answered in time"))
                }
            }
        }

        override fun transferChanged(update: TransferUpdate) {
            merge(update)
            val state = update.state ?: return
            if (state != TransferState.WAITING_FOR_ACCEPT) {
                // Answered, cancelled by the sender, or expired: the request is gone.
                requests.update { q -> q.filter { it.id != update.id } }
                Notifications.dismissRequest(app, update.id)
            }
            if (state in ACTIVE) return
            if (state == TransferState.CANCELLED && update.reason != "Declined" && update.reason != "Not answered in time") {
                transfers.value.firstOrNull { it.id == update.id }?.let { Notifications.cancelled(app, it) }
            }
            val saved = update.saved
            if (update.state == TransferState.COMPLETED && saved != null) {
                scope.launch(Dispatchers.IO) {
                    // Received files are verified in app storage, then published to Downloads/RustDrop.
                    if (!Files.exportToDownloads(app, received, saved)) {
                        merge(update(update.id, TransferState.FAILED, "Received, but couldn't save to Downloads. Free up space and try again."))
                    }
                }
            }
        }

        override fun clipboardReceived(text: String, from: String) {
            // Writing the clipboard is allowed from the background; only reading is restricted.
            main.post {
                app.getSystemService(ClipboardManager::class.java)
                    .setPrimaryClip(ClipData.newPlainText("MaxxDrop", text))
            }
            clipboardFrom.value = from
            scope.launch {
                delay(3000)
                if (clipboardFrom.value == from) clipboardFrom.value = null
            }
        }

        override fun paired(name: String) {
            pairingOffer.value = null
            pairingMessage.value = "Paired with $name."
            trusted.value = node.trusted()
        }

        override fun failed(message: String) {
            fatal.value = message
        }
    }

    private fun watchForegroundService() = scope.launch {
        transfers.map { list -> list.any { it.state in ACTIVE } }.distinctUntilChanged().collect { active ->
            val intent = Intent(app, TransferService::class.java)
            if (active) {
                // Fails when the app is in the background on Android 12+; the transfer still runs while the process lives.
                runCatching { app.startForegroundService(intent) }
            } else {
                app.stopService(intent)
            }
        }
    }
}

class RustDropApp : Application() {
    override fun onCreate() {
        super.onCreate()
        Core.start(this)
    }
}
