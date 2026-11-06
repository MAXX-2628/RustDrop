package app.rustdrop

import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

/** Keeps the process alive while a transfer runs; its notification shows live progress and Cancel. */
class TransferService : Service() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        Notifications.createChannels(this)
        val manager = getSystemService(NotificationManager::class.java)
        scope.launch {
            Core.transfers.map { list -> list.firstOrNull { it.state in ACTIVE } }
                .distinctUntilChanged()
                .collect { t -> manager.notify(Notifications.TRANSFER_ID, Notifications.transfer(this@TransferService, t)) }
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val current = Core.transfers.value.firstOrNull { it.state in ACTIVE }
        startForeground(
            Notifications.TRANSFER_ID,
            Notifications.transfer(this, current),
            ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC,
        )
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }
}
