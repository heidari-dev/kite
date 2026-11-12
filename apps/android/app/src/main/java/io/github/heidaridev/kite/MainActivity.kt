package io.github.heidaridev.kite

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.widget.Button
import android.widget.TextView

class MainActivity : Activity() {

    private val handler = Handler(Looper.getMainLooper())
    private lateinit var status: TextView
    private lateinit var counters: TextView
    private lateinit var toggle: Button

    private val refresh = object : Runnable {
        override fun run() {
            show()
            handler.postDelayed(this, 1000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        status = findViewById(R.id.status)
        counters = findViewById(R.id.counters)
        toggle = findViewById(R.id.toggle)
        toggle.setOnClickListener {
            if (RelayService.relay == null) start() else stop()
            handler.postDelayed({ show() }, 200)
        }
    }

    override fun onResume() {
        super.onResume()
        handler.post(refresh)
    }

    override fun onPause() {
        handler.removeCallbacks(refresh)
        super.onPause()
    }

    private fun start() {
        // The relay runs without it; this only lets the ongoing notification show.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        }
        startForegroundService(Intent(this, RelayService::class.java))
    }

    private fun stop() {
        startService(Intent(this, RelayService::class.java).setAction(RelayService.ACTION_STOP))
    }

    private fun show() {
        val relay = RelayService.relay
        val stats = try {
            relay?.stats()
        } catch (e: IllegalStateException) {
            null // stopped between the check and the call
        }
        if (stats == null) {
            status.text = RelayService.error?.let { getString(R.string.status_error, it) }
                ?: getString(R.string.status_stopped)
            counters.text = ""
            toggle.setText(R.string.start)
        } else {
            status.text = resources.getQuantityString(
                R.plurals.status_running, stats.tunnels.toInt(), stats.tunnels.toInt(),
            )
            counters.text = getString(
                R.string.counters, size(stats.bytesOut.toLong()), size(stats.bytesIn.toLong()),
            )
            toggle.setText(R.string.stop)
        }
    }

    private fun size(bytes: Long): String = when {
        bytes >= 1L shl 30 -> "%.1f GB".format(bytes / (1L shl 30).toDouble())
        bytes >= 1L shl 20 -> "%.1f MB".format(bytes / (1L shl 20).toDouble())
        bytes >= 1L shl 10 -> "%.1f KB".format(bytes / (1L shl 10).toDouble())
        else -> "$bytes B"
    }
}
