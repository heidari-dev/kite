package io.github.heidaridev.kite

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import io.github.heidaridev.kite.ffi.Relay
import io.github.heidaridev.kite.ffi.RelayException
import kotlin.concurrent.thread

/**
 * Keeps the relay running while the screen is off. The relay opens its sockets in this process,
 * so they go wherever the phone's own traffic goes, VPN included.
 */
class RelayService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        startInForeground()
        if (relay == null) {
            try {
                relay = Relay.start(SOCKET_NAME)
                error = null
            } catch (e: RelayException) {
                error = e.message
                stopSelf()
            }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        val running = relay
        relay = null
        // Stopping waits for the relay thread, so keep it off the main thread.
        if (running != null) thread(name = "kite-stop") { running.close() }
        super.onDestroy()
    }

    private fun startInForeground() {
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.channel_name), NotificationManager.IMPORTANCE_LOW),
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, RelayService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(getString(R.string.notification_text))
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, getString(R.string.stop), stop).build())
            .setOngoing(true)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    companion object {
        /** The abstract socket `kite connect --usb` opens through adb. */
        const val SOCKET_NAME = "kite"
        const val ACTION_STOP = "io.github.heidaridev.kite.STOP"
        private const val CHANNEL = "relay"
        private const val NOTIFICATION_ID = 1

        @Volatile
        var relay: Relay? = null
            private set

        @Volatile
        var error: String? = null
            private set
    }
}
