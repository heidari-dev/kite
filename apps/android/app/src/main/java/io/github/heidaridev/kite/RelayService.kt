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
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.os.SystemClock
import android.text.format.Formatter
import io.github.heidaridev.kite.ffi.Relay
import io.github.heidaridev.kite.ffi.Stats
import java.util.concurrent.Executors

/**
 * Keeps the relay running in the background. The relay opens its sockets in this process, so they
 * go wherever the phone's own traffic goes, VPN included.
 */
class RelayService : Service() {

    private val handler = Handler(Looper.getMainLooper())
    private var shown = ""
    private var previous: Stats? = null
    private var previousAt = 0L

    private val tick = object : Runnable {
        override fun run() {
            handler.postDelayed(this, update())
        }
    }

    private val open by lazy {
        PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
    }

    private val stop by lazy {
        PendingIntent.getService(
            this, 1, Intent(this, RelayService::class.java).setAction(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE,
        )
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopSelf()
            return START_NOT_STICKY
        }
        try {
            startInForeground()
        } catch (e: RuntimeException) {
            // Android 12+ refuses a start from the background, and 14+ a missing type permission.
            fail(e)
            return START_NOT_STICKY
        }
        handler.removeCallbacks(tick)
        handler.post(tick)
        val context = applicationContext
        worker.execute {
            if (relay != null) return@execute
            try {
                val key = Store.privateKey(context)
                val peers = Store.peers(context)
                try {
                    relay = Relay.start(SOCKET_NAME, key, peers)
                } finally {
                    key.fill(0)
                }
                paired = peers
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
        handler.removeCallbacks(tick)
        val context = applicationContext
        worker.execute {
            guarded { save(context) }
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

    /**
     * Refreshes the notification when its text changed and returns when to look again: every
     * second while bytes move, every few seconds otherwise.
     */
    private fun update(): Long {
        val stats = try {
            relay?.stats()
        } catch (e: IllegalStateException) {
            null // stopped between the check and the call
        }
        var moved = false
        val text = if (stats == null) {
            getString(R.string.notification_starting)
        } else {
            val now = SystemClock.elapsedRealtime()
            val elapsed = now - previousAt
            val last = previous?.takeIf { elapsed > 0 }
            previous = stats
            previousAt = now
            // From the computer's side: it downloads what the relay sends it.
            val down = if (last == null) 0L else (stats.bytesOut - last.bytesOut).toLong() * 1000 / elapsed
            val up = if (last == null) 0L else (stats.bytesIn - last.bytesIn).toLong() * 1000 / elapsed
            moved = down > 0 || up > 0
            val computers = stats.tunnels.toInt()
            if (computers == 0) {
                getString(R.string.notification_waiting)
            } else {
                resources.getQuantityString(
                    R.plurals.notification_active, computers, computers, speed(down), speed(up),
                )
            }
        }
        if (text != shown) {
            shown = text
            getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(text))
        }
        // A pairing that finished while the screen was off must reach the disk too.
        if (stats != null) savePeers(this)
        return if (moved || stats == null) BUSY_MS else IDLE_MS
    }

    private fun speed(bytesPerSecond: Long) =
        getString(R.string.speed, Formatter.formatFileSize(this, bytesPerSecond))

    private fun notification(text: String): Notification {
        val builder = Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(getString(R.string.notification_title))
            .setContentText(text)
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, getString(R.string.stop), stop).build())
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .setVisibility(Notification.VISIBILITY_PRIVATE)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            builder.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE)
        }
        return builder.build()
    }

    private fun startInForeground() {
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.channel_name), NotificationManager.IMPORTANCE_LOW),
        )
        shown = getString(R.string.notification_starting)
        val notification = notification(shown)
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
        private const val BUSY_MS = 1000L
        private const val IDLE_MS = 5000L

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

        /** The paired computers' keys as last known and written; the relay's list while it runs. */
        @Volatile
        var paired: List<ByteArray> = emptyList()
            private set

        /** Reads the stored computers once, so the list shows before the first start. */
        fun load(context: Context) {
            val app = context.applicationContext
            worker.execute {
                guarded { if (relay == null) paired = Store.peers(app) }
            }
        }

        /** Writes the paired computers if a pairing or removal changed them. */
        fun savePeers(context: Context) {
            val app = context.applicationContext
            worker.execute { guarded { save(app) } }
        }

        /** Unpairs a computer, running or not. A tunnel it has up keeps running until it closes. */
        fun removePeer(context: Context, key: ByteArray) {
            val app = context.applicationContext
            worker.execute {
                guarded {
                    val running = relay
                    if (running != null) {
                        running.removePeer(key)
                        save(app)
                    } else {
                        val rest = Store.peers(app).filterNot { it.contentEquals(key) }
                        Store.savePeers(app, rest)
                        paired = rest
                    }
                }
            }
        }

        /** A failed read or write of the stored computers must not end the process. */
        private fun guarded(block: () -> Unit) {
            try {
                block()
            } catch (e: Exception) {
                error = e.message ?: e.javaClass.simpleName
            }
        }

        private fun save(context: Context) {
            val peers = relay?.peers() ?: return
            val saved = paired
            if (peers.size == saved.size && peers.zip(saved).all { (a, b) -> a.contentEquals(b) }) return
            Store.savePeers(context, peers)
            paired = peers
        }
    }
}
