package app.rustdrop.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.core.MutableTransitionState
import androidx.compose.animation.core.tween
import androidx.compose.animation.fadeIn
import androidx.compose.animation.slideInVertically
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import app.rustdrop.ACTIVE
import app.rustdrop.Core
import app.rustdrop.Transfer
import app.rustdrop.core.IncomingRequest
import app.rustdrop.core.Me
import app.rustdrop.core.PeerInfo
import app.rustdrop.core.TransferState
import app.rustdrop.core.TrustedInfo
import com.composables.icons.lucide.CircleAlert
import com.composables.icons.lucide.CircleCheck
import com.composables.icons.lucide.Clock
import com.composables.icons.lucide.Copy
import com.composables.icons.lucide.Download
import com.composables.icons.lucide.Files
import com.composables.icons.lucide.Folder
import com.composables.icons.lucide.History
import com.composables.icons.lucide.House
import com.composables.icons.lucide.Image
import com.composables.icons.lucide.Lucide
import com.composables.icons.lucide.Monitor
import com.composables.icons.lucide.RefreshCw
import com.composables.icons.lucide.Send
import com.composables.icons.lucide.Settings
import com.composables.icons.lucide.Shield
import com.composables.icons.lucide.ShieldCheck
import com.composables.icons.lucide.Smartphone
import com.composables.icons.lucide.X
import kotlinx.coroutines.delay

enum class PickKind { Media, Files, Folder }

/** Everything the UI can ask for; implemented by MainActivity. */
class Actions(
    val pick: (PickKind, String?) -> Unit,
    val send: (String, List<String>) -> Unit,
    val clearStaged: () -> Unit,
    val refresh: () -> Unit,
    val cancel: (String) -> Unit,
    val respond: (IncomingRequest, Boolean) -> Unit,
    /** Returns an error message, or null on success. */
    val pair: (String, String) -> String?,
    val unpair: (String) -> Unit,
    val copy: (String) -> Unit,
    val openDownloads: () -> Unit,
)

private enum class Tab(val label: String, val icon: ImageVector) {
    Home("Home", Lucide.House),
    Activity("Activity", Lucide.History),
    Settings("Settings", Lucide.Settings),
}

@Composable
fun RustDropUi(actions: Actions) {
    val me by Core.me.collectAsStateWithLifecycle()
    val peers by Core.peers.collectAsStateWithLifecycle()
    val trusted by Core.trusted.collectAsStateWithLifecycle()
    val transfers by Core.transfers.collectAsStateWithLifecycle()
    val requests by Core.requests.collectAsStateWithLifecycle()
    val fatal by Core.fatal.collectAsStateWithLifecycle()
    val staged by Core.staged.collectAsStateWithLifecycle()
    val preparing by Core.preparing.collectAsStateWithLifecycle()
    val notice by Core.notice.collectAsStateWithLifecycle()

    var tab by rememberSaveable { mutableStateOf(Tab.Home) }
    var pairName by rememberSaveable { mutableStateOf("") }
    val sp = LocalSpacing.current

    Scaffold(
        bottomBar = {
            NavigationBar(containerColor = LocalPalette.current.surface) {
                Tab.entries.forEach { t ->
                    NavigationBarItem(
                        selected = tab == t,
                        onClick = { tab = t },
                        icon = { Icon(t.icon, contentDescription = null) },
                        label = { Text(t.label) },
                    )
                }
            }
        },
    ) { padding ->
        Column(Modifier.padding(padding).fillMaxSize()) {
            fatal?.let { Banner("RustDrop can't receive files right now: $it", Modifier.padding(sp.lg)) }
            when (tab) {
                Tab.Home -> HomeScreen(
                    me = me,
                    peers = peers,
                    staged = staged,
                    preparing = preparing,
                    notice = notice,
                    transfers = transfers,
                    actions = actions,
                    onPairDevice = { name ->
                        pairName = name
                        tab = Tab.Settings
                    },
                    onSeeAll = { tab = Tab.Activity },
                )
                Tab.Activity -> ActivityScreen(transfers, actions)
                Tab.Settings -> me?.let { SettingsScreen(it, trusted, peers, pairName, { pairName = it }, actions) }
            }
        }
    }

    requests.firstOrNull()?.let { IncomingDialog(it, actions.respond) }
}

@Composable
private fun HomeScreen(
    me: Me?,
    peers: List<PeerInfo>,
    staged: List<String>,
    preparing: Boolean,
    notice: String?,
    transfers: List<Transfer>,
    actions: Actions,
    onPairDevice: (String) -> Unit,
    onSeeAll: () -> Unit,
) {
    val sp = LocalSpacing.current
    val palette = LocalPalette.current
    var searching by remember { mutableStateOf(true) }
    LaunchedEffect(Unit) {
        delay(4000)
        searching = false
    }

    LazyColumn(
        contentPadding = PaddingValues(sp.xl),
        verticalArrangement = Arrangement.spacedBy(sp.xxl),
    ) {
        item {
            Column(verticalArrangement = Arrangement.spacedBy(sp.xs)) {
                Text(me?.name ?: "This phone", style = MaterialTheme.typography.headlineMedium, modifier = Modifier.semantics { heading() })
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(sp.sm)) {
                    Box(Modifier.size(8.dp).background(palette.success, CircleShape))
                    Text("Ready to receive from paired devices", color = palette.textSecondary)
                }
            }
        }
        item { SendActions(actions, null) }
        if (preparing) item { StatusLine(Lucide.Clock, "Preparing files...") }
        if (staged.isNotEmpty()) {
            item {
                Row(
                    Modifier.fillMaxWidth()
                        .border(1.dp, palette.primary, RoundedCornerShape(LocalRadius.current.card))
                        .padding(start = sp.lg, end = sp.xs, top = sp.xs, bottom = sp.xs),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Icon(Lucide.Files, contentDescription = null)
                    Text(
                        "${plural(staged.size, "item")} ready. Tap a device to send ${if (staged.size == 1) "it" else "them"}.",
                        Modifier.weight(1f).padding(horizontal = sp.md),
                    )
                    TextButton(onClick = actions.clearStaged) { Text("Clear") }
                }
            }
        }
        notice?.let { item { StatusLine(Lucide.CircleAlert, it, palette.error) } }

        item {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text("Nearby devices", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f).semantics { heading() })
                TextButton(onClick = actions.refresh) {
                    Icon(Lucide.RefreshCw, contentDescription = null, Modifier.size(16.dp))
                    Spacer(Modifier.width(sp.sm))
                    Text("Refresh")
                }
            }
        }
        if (peers.isEmpty()) {
            item {
                if (searching) {
                    Empty(Lucide.Smartphone, "Looking for nearby devices...", null, progress = true)
                } else {
                    Empty(
                        Lucide.Smartphone,
                        "No devices nearby",
                        "Make sure both devices are connected to the same network and RustDrop is running.",
                    ) {
                        OutlinedButton(onClick = actions.refresh) { Text("Refresh") }
                    }
                }
            }
        } else {
            items(peers, key = { it.id }) { peer ->
                DeviceRow(peer) {
                    when {
                        peer.trusted == false -> onPairDevice(peer.name)
                        peer.trusted == null -> Unit
                        staged.isNotEmpty() -> actions.send(peer.id, staged)
                        else -> actions.pick(PickKind.Files, peer.id)
                    }
                }
            }
        }

        val recent = transfers.take(3)
        if (recent.isNotEmpty()) {
            item {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text("Recent activity", style = MaterialTheme.typography.titleLarge, modifier = Modifier.weight(1f).semantics { heading() })
                    TextButton(onClick = onSeeAll) { Text("See all") }
                }
            }
            items(recent, key = { "recent-" + it.id }) { TransferRow(it, actions) }
        }
    }
}

@Composable
private fun SendActions(actions: Actions, device: String?) {
    val sp = LocalSpacing.current
    var menu by remember { mutableStateOf(false) }
    Row(horizontalArrangement = Arrangement.spacedBy(sp.md)) {
        Box(Modifier.weight(1f)) {
            Button(onClick = { menu = true }, modifier = Modifier.fillMaxWidth().heightIn(min = 56.dp)) {
                Icon(Lucide.Send, contentDescription = null)
                Spacer(Modifier.width(sp.sm))
                Text("Send")
            }
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(
                    text = { Text("Photos and videos") },
                    leadingIcon = { Icon(Lucide.Image, contentDescription = null) },
                    onClick = { menu = false; actions.pick(PickKind.Media, device) },
                )
                DropdownMenuItem(
                    text = { Text("Files") },
                    leadingIcon = { Icon(Lucide.Files, contentDescription = null) },
                    onClick = { menu = false; actions.pick(PickKind.Files, device) },
                )
                DropdownMenuItem(
                    text = { Text("A folder") },
                    leadingIcon = { Icon(Lucide.Folder, contentDescription = null) },
                    onClick = { menu = false; actions.pick(PickKind.Folder, device) },
                )
            }
        }
        OutlinedButton(onClick = actions.openDownloads, modifier = Modifier.weight(1f).heightIn(min = 56.dp)) {
            Icon(Lucide.Download, contentDescription = null)
            Spacer(Modifier.width(sp.sm))
            Text("Received")
        }
    }
}

@Composable
private fun DeviceRow(peer: PeerInfo, onClick: () -> Unit) {
    val sp = LocalSpacing.current
    val palette = LocalPalette.current
    // New devices fade and slide in once; devices already shown are not re-animated.
    val shown = remember { MutableTransitionState(false).apply { targetState = true } }
    val status = when (peer.trusted) {
        null -> "Checking"
        true -> "Paired"
        false -> "Not paired. Tap to pair."
    }
    val action = if (peer.trusted == false) "Pair with ${peer.name}" else "Send to ${peer.name}"
    AnimatedVisibility(shown, enter = fadeIn(tween(220)) + slideInVertically(tween(220)) { it / 4 }) {
        Row(
            Modifier.fillMaxWidth()
                .background(palette.surface, RoundedCornerShape(LocalRadius.current.card))
                .clickable(role = Role.Button, onClick = onClick)
                .semantics(mergeDescendants = true) { contentDescription = "$action. ${platformName(peer.platform)}, $status" }
                .padding(sp.lg),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(sp.lg),
        ) {
            Avatar(if (peer.platform == "android") Lucide.Smartphone else Lucide.Monitor, peer.trusted == true)
            Column(Modifier.weight(1f)) {
                Text(peer.name, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(platformName(peer.platform), style = MaterialTheme.typography.bodySmall, color = palette.textSecondary)
            }
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(sp.xs)) {
                when (peer.trusted) {
                    null -> CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
                    true -> Icon(Lucide.ShieldCheck, contentDescription = null, Modifier.size(16.dp), tint = palette.success)
                    false -> Icon(Lucide.Shield, contentDescription = null, Modifier.size(16.dp), tint = palette.textSecondary)
                }
                Text(
                    if (peer.trusted == false) "Not paired" else status,
                    style = MaterialTheme.typography.labelSmall,
                    color = if (peer.trusted == true) palette.success else palette.textSecondary,
                )
            }
        }
    }
}

@Composable
private fun ActivityScreen(transfers: List<Transfer>, actions: Actions) {
    val sp = LocalSpacing.current
    LazyColumn(contentPadding = PaddingValues(sp.xl), verticalArrangement = Arrangement.spacedBy(sp.md)) {
        item { Text("Activity", style = MaterialTheme.typography.headlineMedium, modifier = Modifier.semantics { heading() }) }
        if (transfers.isEmpty()) {
            item { Empty(Lucide.History, "Nothing shared yet", "Your recent transfers will appear here.") }
        } else {
            items(transfers, key = { it.id }) { TransferRow(it, actions) }
        }
    }
}

@Composable
private fun TransferRow(t: Transfer, actions: Actions) {
    val sp = LocalSpacing.current
    val palette = LocalPalette.current
    val pct = if (t.total > 0) ((t.done * 100) / t.total).toInt().coerceIn(0, 100) else 0
    val seconds = t.flowingAt?.let { (System.currentTimeMillis() - it) / 1000.0 } ?: 0.0
    val speed = if (seconds > 0.5) t.done / seconds else 0.0
    val left = if (speed > 0) (t.total - t.done) / speed else null
    val title = if (t.outgoing) "To ${t.peer}" else "From ${t.peer}"
    val size = if (t.total > 0) "${plural(t.items.toInt(), "item")}, ${formatBytes(t.total.toDouble())}" else plural(t.items.toInt(), "item")

    val (icon, color) = when (t.state) {
        TransferState.WAITING_FOR_ACCEPT -> Lucide.Clock to palette.warning
        TransferState.TRANSFERRING, TransferState.VERIFYING -> Lucide.ShieldCheck to palette.primary
        TransferState.COMPLETED -> Lucide.CircleCheck to palette.success
        TransferState.FAILED -> Lucide.CircleAlert to palette.error
        TransferState.CANCELLED -> Lucide.X to palette.textSecondary
    }
    val status = when (t.state) {
        TransferState.WAITING_FOR_ACCEPT -> if (t.outgoing) "Waiting for ${t.peer} to accept" else "Waiting for you to accept"
        TransferState.TRANSFERRING -> buildString {
            append(if (t.outgoing) "Sending $pct%" else "Receiving $pct%")
            if (speed > 0) append(", ${formatBytes(speed)}/s")
            left?.let { append(", ${formatDuration(it)} left") }
        }
        TransferState.VERIFYING -> "Verifying files"
        TransferState.COMPLETED -> if (t.outgoing) "Sent and verified" else "Received and verified. Saved to Downloads/RustDrop."
        TransferState.FAILED -> t.reason?.let { "Transfer failed. $it" } ?: "Transfer failed"
        TransferState.CANCELLED -> t.reason ?: "Cancelled"
    }

    Column(
        Modifier.fillMaxWidth()
            .background(palette.surface, RoundedCornerShape(LocalRadius.current.card))
            .padding(sp.lg),
        verticalArrangement = Arrangement.spacedBy(sp.sm),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(sp.md)) {
            Icon(if (t.outgoing) Lucide.Send else Lucide.Download, contentDescription = null, tint = palette.textSecondary)
            Text(title, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(size, style = MaterialTheme.typography.bodySmall, color = palette.textSecondary)
        }
        if (t.state == TransferState.TRANSFERRING || t.state == TransferState.VERIFYING) {
            LinearProgressIndicator(
                progress = { pct / 100f },
                modifier = Modifier.fillMaxWidth().semantics { contentDescription = "$title, $pct percent" },
                color = palette.primary,
                trackColor = palette.surfaceElevated,
            )
        }
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(sp.xs)) {
            Icon(icon, contentDescription = null, Modifier.size(14.dp), tint = color)
            Text(
                status,
                style = MaterialTheme.typography.bodySmall,
                color = if (t.state == TransferState.FAILED) palette.error else palette.textSecondary,
                modifier = Modifier.weight(1f),
            )
            if (t.outgoing && t.state in ACTIVE) {
                TextButton(onClick = { actions.cancel(t.id) }) { Text("Cancel") }
            }
            if (!t.outgoing && t.state == TransferState.COMPLETED) {
                TextButton(onClick = actions.openDownloads) { Text("Open Downloads") }
            }
        }
    }
}

@Composable
private fun IncomingDialog(request: IncomingRequest, respond: (IncomingRequest, Boolean) -> Unit) {
    val sp = LocalSpacing.current
    val palette = LocalPalette.current
    val count = request.count.toInt()
    val shown = request.files.take(6)
    AlertDialog(
        // Back or tapping outside declines; nothing is ever accepted by default.
        onDismissRequest = { respond(request, false) },
        icon = { Icon(if (request.platform == "android") Lucide.Smartphone else Lucide.Monitor, contentDescription = null, tint = palette.primary) },
        title = { Text("${request.sender} wants to send you ${if (count == 1) "a file" else "$count files"}", textAlign = TextAlign.Center) },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(sp.sm)) {
                Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(sp.xs)) {
                    Icon(Lucide.ShieldCheck, contentDescription = null, Modifier.size(16.dp), tint = palette.success)
                    Text("Paired ${platformName(request.platform)} device. ${formatBytes(request.total.toDouble())} in total.")
                }
                Column(
                    Modifier.fillMaxWidth().background(palette.surface, RoundedCornerShape(LocalRadius.current.input)).padding(sp.md),
                    verticalArrangement = Arrangement.spacedBy(sp.xs),
                ) {
                    shown.forEach { f ->
                        Row {
                            Text(f.path, Modifier.weight(1f), maxLines = 1, overflow = TextOverflow.Ellipsis)
                            Text(formatBytes(f.size.toDouble()), color = palette.textSecondary)
                        }
                    }
                    if (count > shown.size) Text("and ${count - shown.size} more", color = palette.textSecondary)
                }
                Text("Files are checked before they're saved to Downloads/RustDrop.", style = MaterialTheme.typography.bodySmall, color = palette.textSecondary)
            }
        },
        confirmButton = {
            Button(onClick = { respond(request, true) }) {
                Icon(Lucide.Download, contentDescription = null, Modifier.size(18.dp))
                Spacer(Modifier.width(sp.sm))
                Text("Accept")
            }
        },
        dismissButton = { OutlinedButton(onClick = { respond(request, false) }) { Text("Decline") } },
    )
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SettingsScreen(
    me: Me,
    trusted: List<TrustedInfo>,
    peers: List<PeerInfo>,
    pairName: String,
    setPairName: (String) -> Unit,
    actions: Actions,
) {
    val sp = LocalSpacing.current
    val palette = LocalPalette.current
    var fingerprint by rememberSaveable { mutableStateOf("") }
    var error by remember { mutableStateOf<String?>(null) }
    var confirming by remember { mutableStateOf<String?>(null) }
    var copied by remember { mutableStateOf(false) }
    LaunchedEffect(copied) {
        if (copied) {
            delay(2000)
            copied = false
        }
    }
    val unpaired = peers.filter { it.trusted == false }

    LazyColumn(contentPadding = PaddingValues(sp.xl), verticalArrangement = Arrangement.spacedBy(sp.xl)) {
        item { Text("Settings", style = MaterialTheme.typography.headlineMedium, modifier = Modifier.semantics { heading() }) }

        item {
            Panel("Your fingerprint") {
                Text("Compare it with what the other device shows before you pair.", style = MaterialTheme.typography.bodySmall, color = palette.textSecondary)
                Text(me.fingerprint.chunked(4).joinToString(" "), fontFamily = FontFamily.Monospace, lineHeight = MaterialTheme.typography.bodyLarge.lineHeight)
                TextButton(onClick = { actions.copy(me.fingerprint); copied = true }) {
                    Icon(Lucide.Copy, contentDescription = null, Modifier.size(16.dp))
                    Spacer(Modifier.width(sp.sm))
                    Text(if (copied) "Copied" else "Copy")
                }
            }
        }

        item {
            Panel("Pair a device") {
                Text(
                    "Paired devices can ask to send you files. You still accept each transfer. Pair on both devices, using each other's fingerprint.",
                    style = MaterialTheme.typography.bodySmall,
                    color = palette.textSecondary,
                )
                if (unpaired.isNotEmpty()) {
                    FlowRow(horizontalArrangement = Arrangement.spacedBy(sp.sm), verticalArrangement = Arrangement.spacedBy(sp.sm)) {
                        unpaired.forEach { p -> OutlinedButton(onClick = { setPairName(p.name) }) { Text(p.name) } }
                    }
                }
                OutlinedTextField(
                    value = pairName,
                    onValueChange = { setPairName(it.take(64)) },
                    label = { Text("Device name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = fingerprint,
                    onValueChange = { fingerprint = it },
                    label = { Text("Their fingerprint") },
                    supportingText = { Text(error ?: "Shown on the other device under Trusted devices or Settings") },
                    isError = error != null,
                    textStyle = MaterialTheme.typography.bodyMedium.copy(fontFamily = FontFamily.Monospace),
                    keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false),
                    minLines = 2,
                    modifier = Modifier.fillMaxWidth(),
                )
                Button(onClick = {
                    error = actions.pair(fingerprint, pairName)
                    if (error == null) {
                        fingerprint = ""
                        setPairName("")
                    }
                }) {
                    Icon(Lucide.ShieldCheck, contentDescription = null, Modifier.size(18.dp))
                    Spacer(Modifier.width(sp.sm))
                    Text("Pair device")
                }
            }
        }

        item {
            Panel("Paired devices") {
                if (trusted.isEmpty()) {
                    Text("No paired devices yet. Pair a device above to start sharing.", color = palette.textSecondary)
                }
                trusted.forEach { t ->
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(sp.md)) {
                        Icon(Lucide.ShieldCheck, contentDescription = null, tint = palette.success)
                        Column(Modifier.weight(1f)) {
                            Text(t.name, fontWeight = FontWeight.SemiBold)
                            Text(t.fingerprint.take(16).chunked(4).joinToString(" ") + "...", style = MaterialTheme.typography.bodySmall, fontFamily = FontFamily.Monospace, color = palette.textSecondary)
                        }
                        if (confirming == t.fingerprint) {
                            OutlinedButton(
                                onClick = { actions.unpair(t.fingerprint); confirming = null },
                                border = BorderStroke(1.dp, palette.error),
                            ) { Text("Remove", color = palette.error) }
                            TextButton(onClick = { confirming = null }) { Text("Keep") }
                        } else {
                            TextButton(onClick = { confirming = t.fingerprint }) { Text("Remove") }
                        }
                    }
                }
            }
        }

        item {
            Panel("Appearance") {
                val current = LocalCurrentTheme.current
                val choose = LocalThemePreference.current
                Column(Modifier.selectableGroup()) {
                    ThemePreference.entries.forEach { option ->
                        Row(
                            Modifier.fillMaxWidth().heightIn(min = 48.dp)
                                .selectable(selected = current == option, onClick = { choose(option) }, role = Role.RadioButton),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            RadioButton(selected = current == option, onClick = null)
                            Spacer(Modifier.width(sp.md))
                            Text(
                                when (option) {
                                    ThemePreference.System -> "Use system setting"
                                    ThemePreference.Dark -> "Dark"
                                    ThemePreference.Light -> "Light"
                                },
                            )
                        }
                    }
                }
            }
        }

        item {
            Panel("This device") {
                Text("Received files are saved to Downloads/RustDrop.", color = palette.textSecondary)
                Text("Name: ${me.name}")
                Text("Version: ${me.version}", color = palette.textSecondary)
            }
        }
    }
}

@Composable
private fun Panel(title: String, content: @Composable () -> Unit) {
    val sp = LocalSpacing.current
    Column(
        Modifier.fillMaxWidth()
            .background(LocalPalette.current.surface, RoundedCornerShape(LocalRadius.current.panel))
            .padding(sp.xl),
        verticalArrangement = Arrangement.spacedBy(sp.md),
    ) {
        Text(title, style = MaterialTheme.typography.titleLarge, modifier = Modifier.semantics { heading() })
        content()
    }
}

@Composable
private fun Avatar(icon: ImageVector, highlighted: Boolean) {
    val palette = LocalPalette.current
    Box(Modifier.size(48.dp).background(palette.surfaceElevated, CircleShape), contentAlignment = Alignment.Center) {
        Icon(icon, contentDescription = null, tint = if (highlighted) palette.primary else palette.textSecondary)
    }
}

@Composable
private fun Empty(icon: ImageVector, title: String, text: String?, progress: Boolean = false, action: (@Composable () -> Unit)? = null) {
    val sp = LocalSpacing.current
    Column(
        Modifier.fillMaxWidth().padding(vertical = sp.xxxl),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(sp.sm),
    ) {
        if (progress) CircularProgressIndicator(Modifier.size(28.dp)) else Avatar(icon, false)
        Text(title, style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center)
        text?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = LocalPalette.current.textSecondary, textAlign = TextAlign.Center) }
        action?.invoke()
    }
}

@Composable
private fun Banner(text: String, modifier: Modifier = Modifier) {
    val palette = LocalPalette.current
    Row(
        modifier.fillMaxWidth().border(1.dp, palette.error, RoundedCornerShape(LocalRadius.current.card)).padding(LocalSpacing.current.md),
        horizontalArrangement = Arrangement.spacedBy(LocalSpacing.current.sm),
    ) {
        Icon(Lucide.CircleAlert, contentDescription = null, tint = palette.error)
        Text(text, color = palette.error)
    }
}

@Composable
private fun StatusLine(icon: ImageVector, text: String, color: androidx.compose.ui.graphics.Color = LocalPalette.current.textSecondary) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(LocalSpacing.current.sm)) {
        Icon(icon, contentDescription = null, Modifier.size(16.dp), tint = color)
        Text(text, color = color)
    }
}

private fun platformName(p: String) = when (p) {
    "windows" -> "Windows"
    "android" -> "Android"
    else -> p
}

private fun plural(n: Int, word: String) = "$n $word${if (n == 1) "" else "s"}"

private fun formatBytes(bytes: Double): String {
    val units = listOf("B", "KB", "MB", "GB", "TB")
    var n = bytes
    var i = 0
    while (n >= 1024 && i < units.size - 1) {
        n /= 1024
        i++
    }
    return if (i == 0) "${n.toLong()} B" else "%.${if (n < 10) 1 else 0}f %s".format(n, units[i])
}

private fun formatDuration(s: Double): String = when {
    s < 60 -> "${maxOf(1, kotlin.math.ceil(s).toInt())} s"
    s < 3600 -> "${kotlin.math.ceil(s / 60).toInt()} min"
    else -> "${(s / 3600).toInt()} h ${kotlin.math.ceil((s % 3600) / 60).toInt()} min"
}
