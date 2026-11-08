package app.rustdrop

import android.content.Context
import android.content.Intent
import android.service.quicksettings.TileService
import androidx.core.content.pm.ShortcutInfoCompat
import androidx.core.content.pm.ShortcutManagerCompat
import androidx.core.graphics.drawable.IconCompat
import app.rustdrop.core.PeerInfo

/**
 * Direct Share: paired devices appear at the top of the Android share sheet. Picking one
 * opens MainActivity with the device id as EXTRA_SHORTCUT_ID, and the files go straight to it.
 */
object Shortcuts {
    /** Matches the share-target category in res/xml/shortcuts.xml. */
    private const val CATEGORY = "app.rustdrop.category.SHARE_TARGET"
    private val pushed = mutableSetOf<String>()

    /** Publishes each paired device that is nearby; long-lived, so Android keeps ranking them. */
    fun update(ctx: Context, paired: List<PeerInfo>) {
        for (peer in paired) {
            if (!pushed.add(peer.id + peer.name)) continue
            val info = ShortcutInfoCompat.Builder(ctx, peer.id)
                .setShortLabel(peer.name)
                .setLongLabel("Send to ${peer.name}")
                .setIcon(IconCompat.createWithResource(ctx, R.mipmap.ic_launcher))
                .setIntent(Intent(ctx, MainActivity::class.java).setAction(Intent.ACTION_DEFAULT))
                .setCategories(setOf(CATEGORY))
                .setLongLived(true)
                .build()
            runCatching { ShortcutManagerCompat.pushDynamicShortcut(ctx, info) }
        }
    }
    // shortcut: an unpaired device stays in the share sheet until Android ranks it out (picking
    // it fails with a notice); remove it on unpair once PeerInfo carries the fingerprint.
}

/** Quick Settings tile: opens Ferrydrop with the file picker, ready to send. */
class SendTile : TileService() {
    // Before Android 14 the Intent overload is the only one there is.
    @android.annotation.SuppressLint("StartActivityAndCollapseDeprecated")
    override fun onClick() {
        super.onClick()
        val intent = Intent(this, MainActivity::class.java)
            .setAction(MainActivity.ACTION_PICK_FILES)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        if (android.os.Build.VERSION.SDK_INT >= 34) {
            val pending = android.app.PendingIntent.getActivity(this, 0, intent, android.app.PendingIntent.FLAG_IMMUTABLE)
            startActivityAndCollapse(pending)
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(intent)
        }
    }
}
