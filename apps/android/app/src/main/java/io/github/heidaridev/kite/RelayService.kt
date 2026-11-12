package io.github.heidaridev.kite

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import io.github.heidaridev.kite.ffi.Relay
import java.util.concurrent.Executors

/**
 * Keeps the relay running in the background. The relay opens its sockets in this process, so they
 * go wherever the phone's own traffic goes, VPN included.
 */
class RelayService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        startInForeground()
        worker.execute {
            if (relay != null) return@execute
            try {
                val key = Store.privateKey(this)
                val peers = Store.peers(this)
                try {
                    relay = Relay.start(SOCKET_NAME, key, peers)
                } finally {
                    key.fill(0)
                }
                saved = peers
                error = null
            } catch (e: Exception) {
                fail(e)
            } catch (e: LinkageError) {
                fail(e) // libkite_ffi.so missing or built for another ABI
            }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        val context = applicationContext
        worker.execute {
            save(context)
            val running = relay
            relay = null
            running?.close()
        }
        super.onDestroy()
    }

    private fun fail(e: Throwable) {
        error = e.message ?: e.javaClass.simpleName
        stopSelf()
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

        /**
         * Starts and stops run here one after another, off the main thread: stopping waits for the
         * relay thread, and a start must not race the socket name a stopping relay still holds.
         */
        private val worker = Executors.newSingleThreadExecutor()

        @Volatile
        var relay: Relay? = null
            private set

        @Volatile
        var error: String? = null
            private set

        /** The paired computers as last written; only touched on [worker]. */
        private var saved: List<ByteArray> = emptyList()

        /** Writes the paired computers if a pairing or removal changed them. */
        fun savePeers(context: Context) {
            val app = context.applicationContext
            worker.execute { save(app) }
        }

        private fun save(context: Context) {
            val peers = relay?.peers() ?: return
            if (peers.size == saved.size && peers.zip(saved).all { (a, b) -> a.contentEquals(b) }) return
            Store.savePeers(context, peers)
            saved = peers
        }
    }
}
