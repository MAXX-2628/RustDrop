package app.rustdrop

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.core.net.toUri
import app.rustdrop.core.IncomingRequest
import app.rustdrop.core.TransferState

/** System notifications: ongoing transfer (a Live Update chip on Android 16), requests, cancellations. */
object Notifications {
    const val TRANSFER_ID = 1
    private const val CH_TRANSFERS = "transfers"
    private const val CH_REQUESTS = "requests"
    private const val CH_EVENTS = "events"
    const val ACTION_CANCEL = "app.rustdrop.CANCEL"
    const val ACTION_DECLINE = "app.rustdrop.DECLINE"
    const val EXTRA_ID = "id"

    fun createChannels(ctx: Context) {
        ctx.getSystemService(NotificationManager::class.java).createNotificationChannels(
            listOf(
                NotificationChannel(CH_TRANSFERS, "Transfers in progress", NotificationManager.IMPORTANCE_LOW).apply {
                    description = "Progress while Ferrydrop sends or receives files"
                },
                NotificationChannel(CH_REQUESTS, "Incoming files", NotificationManager.IMPORTANCE_HIGH).apply {
                    description = "A paired device wants to send you files"
                },
                NotificationChannel(CH_EVENTS, "Cancelled transfers", NotificationManager.IMPORTANCE_DEFAULT).apply {
                    description = "When you or the other device cancels a transfer"
                },
            ),
        )
    }

    private fun openApp(ctx: Context) = PendingIntent.getActivity(
        ctx, 0, Intent(ctx, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE,
    )

    private fun action(ctx: Context, action: String, id: String) = PendingIntent.getBroadcast(
        ctx, (action + id).hashCode(),
        Intent(ctx, ActionReceiver::class.java).setAction(action).putExtra(EXTRA_ID, id),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    /** The foreground-service notification for the first active transfer. */
    fun transfer(ctx: Context, t: Transfer?): Notification {
        val b = Notification.Builder(ctx, CH_TRANSFERS)
            .setSmallIcon(if (t?.outgoing == true) android.R.drawable.stat_sys_upload else android.R.drawable.stat_sys_download)
            .setContentIntent(openApp(ctx))
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_PROGRESS)
        if (t == null) return b.setContentTitle("Preparing transfer").build()

        val pct = if (t.total > 0) ((t.done * 100) / t.total).toInt().coerceIn(0, 100) else 0
        val verb = if (t.outgoing) "Sending to" else "Receiving from"
        b.setContentTitle("$verb ${t.peer}")
            .setContentText(
                when (t.state) {
                    TransferState.WAITING_FOR_ACCEPT -> "Waiting for ${t.peer} to accept"
                    TransferState.VERIFYING -> "Verifying files"
                    else -> "$pct%"
                },
            )
            .addAction(Notification.Action.Builder(null, "Cancel", action(ctx, ACTION_CANCEL, t.id)).build())
        if (Build.VERSION.SDK_INT >= 36) {
            // Android 16 Live Update: shown as a status-bar chip, like an island.
            b.setStyle(Notification.ProgressStyle().setProgress(pct).setProgressIndeterminate(t.state == TransferState.WAITING_FOR_ACCEPT))
                .setShortCriticalText("$pct%")
                // The extra (API 36) instead of setRequestPromotedOngoing, which needs 36.1.
                .addExtras(Bundle().apply { putBoolean(Notification.EXTRA_REQUEST_PROMOTED_ONGOING, true) })
        } else {
            b.setProgress(100, pct, t.state == TransferState.WAITING_FOR_ACCEPT)
        }
        return b.build()
    }

    fun incoming(ctx: Context, request: IncomingRequest) {
        val count = request.count.toInt()
        val n = Notification.Builder(ctx, CH_REQUESTS)
            .setSmallIcon(android.R.drawable.stat_sys_download)
            .setContentTitle("${request.sender} wants to send you ${if (count == 1) "a file" else "$count files"}")
            .setContentText("Open Ferrydrop to see the files and accept.")
            .setCategory(Notification.CATEGORY_CALL)
            .setContentIntent(openApp(ctx))
            .setAutoCancel(true)
            .setTimeoutAfter(60_000) // the request expires after a minute
            .addAction(Notification.Action.Builder(null, "Decline", action(ctx, ACTION_DECLINE, request.id)).build())
            .build()
        post(ctx, request.id.hashCode(), n)
    }

    fun dismissRequest(ctx: Context, id: String) {
        ctx.getSystemService(NotificationManager::class.java).cancel(id.hashCode())
    }

    fun cancelled(ctx: Context, t: Transfer) {
        val text = when (t.reason) {
            "Cancelled by the other device" -> "${t.peer} cancelled the transfer."
            else -> "You cancelled the transfer with ${t.peer}."
        }
        val n = Notification.Builder(ctx, CH_EVENTS)
            .setSmallIcon(android.R.drawable.stat_notify_error)
            .setContentTitle("Transfer cancelled")
            .setContentText(text)
            .setContentIntent(openApp(ctx))
            .setAutoCancel(true)
            .build()
        post(ctx, t.id.hashCode(), n)
    }

    /** A link or text a paired device sent on purpose: Open (links only) and Copy. Never opened by itself. */
    fun text(ctx: Context, text: String, from: String) {
        val link = isLink(text)
        val copy = PendingIntent.getBroadcast(
            ctx, ("copy" + text).hashCode(),
            Intent(ctx, ActionReceiver::class.java).setAction(ACTION_COPY).putExtra(EXTRA_TEXT, text),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val b = Notification.Builder(ctx, CH_REQUESTS)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle("$from sent you ${if (link) "a link" else "text"}")
            .setContentText(text.take(200))
            .setStyle(Notification.BigTextStyle().bigText(text.take(1000)))
            .setContentIntent(openApp(ctx))
            .setAutoCancel(true)
            .addAction(Notification.Action.Builder(null, "Copy", copy).build())
        if (link) {
            val open = PendingIntent.getActivity(
                ctx, text.hashCode(),
                Intent(Intent.ACTION_VIEW, text.toUri()).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK),
                PendingIntent.FLAG_IMMUTABLE,
            )
            b.addAction(Notification.Action.Builder(null, "Open", open).build())
        }
        post(ctx, ("text" + text).hashCode(), b.build())
    }

    const val ACTION_COPY = "app.rustdrop.COPY"
    const val EXTRA_TEXT = "text"

    private fun post(ctx: Context, id: Int, n: Notification) {
        // Throws or is ignored when notifications are not allowed; the in-app island still shows it.
        runCatching { ctx.getSystemService(NotificationManager::class.java).notify(id, n) }
    }
}

/** Only plain web links get an Open button; anything else is just text to copy. */
fun isLink(text: String) =
    (text.startsWith("https://") || text.startsWith("http://")) && text.length < 4096 && text.none { it.isWhitespace() || it == '"' }

/** Cancel / Decline / Copy buttons in notifications. Not exported: only our own PendingIntents reach it. */
class ActionReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action == Notifications.ACTION_COPY) {
            val text = intent.getStringExtra(Notifications.EXTRA_TEXT) ?: return
            context.getSystemService(android.content.ClipboardManager::class.java)
                .setPrimaryClip(android.content.ClipData.newPlainText("Ferrydrop", text))
            return
        }
        val id = intent.getStringExtra(Notifications.EXTRA_ID) ?: return
        if (!Core.started) return
        when (intent.action) {
            Notifications.ACTION_CANCEL -> Core.node.cancel(id)
            Notifications.ACTION_DECLINE -> Core.decline(id)
        }
    }
}
