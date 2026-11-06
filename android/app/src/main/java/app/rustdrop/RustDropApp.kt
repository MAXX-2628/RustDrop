package app.rustdrop

import android.app.Application
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import android.provider.Settings
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
    /** Files copied from pickers or the share sheet, waiting for a device. */
    val staged = MutableStateFlow<List<String>>(emptyList())
    val preparing = MutableStateFlow(false)
    val notice = MutableStateFlow<String?>(null)

    /** QR pairing: the code this phone is showing, and the outcome to report. */
    val pairingOffer = MutableStateFlow<PairingOffer?>(null)
    val pairingMessage = MutableStateFlow<String?>(null)

    /** Clipboard sync (opt-in), and the device the last clipboard text came from (shown for 3 s). */
    val clipboardSync = MutableStateFlow(false)
    val clipboardFrom = MutableStateFlow<String?>(null)

    private val outgoingDirs = mutableMapOf<String, File>()
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

    fun send(device: String, paths: List<String>) {
        try {
            val id = node.send(device, paths)
            File(paths.first()).parentFile?.let { synchronized(outgoingDirs) { outgoingDirs[id] = it } }
            staged.value = emptyList()
            notice.value = null
        } catch (e: NodeException) {
            notice.value = e.message
        }
    }

    fun clearStaged() {
        staged.value.firstOrNull()?.let { File(it).parentFile?.deleteRecursively() }
        staged.value = emptyList()
    }

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
            items = u.items?.toLong() ?: old.items,
            total = u.total?.toLong() ?: old.total,
            done = u.done?.toLong() ?: old.done,
            state = u.state ?: old.state,
            reason = u.reason ?: old.reason,
            savedCount = u.saved?.size ?: old.savedCount,
        )
        if (t.state == TransferState.TRANSFERRING && t.flowingAt == null) t = t.copy(flowingAt = now)
        if (i >= 0) list.toMutableList().also { it[i] = t } else (listOf(t) + list).take(200)
    }

    private fun update(id: String, state: TransferState, reason: String?) =
        TransferUpdate(id, null, null, null, null, null, state, reason, null)

    /** Receives core events on Rust threads; flows are thread-safe. */
    private object Events : Listener {
        override fun peersChanged(peers: List<PeerInfo>) {
            Core.peers.value = peers
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
            synchronized(outgoingDirs) { outgoingDirs.remove(update.id) }?.deleteRecursively()
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
                    .setPrimaryClip(ClipData.newPlainText("RustDrop", text))
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
