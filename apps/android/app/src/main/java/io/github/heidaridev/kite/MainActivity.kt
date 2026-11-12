package io.github.heidaridev.kite

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.format.Formatter
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.TextView

class MainActivity : Activity() {

    private val handler = Handler(Looper.getMainLooper())
    private lateinit var status: TextView
    private lateinit var counters: TextView
    private lateinit var toggle: Button
    private lateinit var pair: Button
    private lateinit var pairing: TextView

    private val refresh = object : Runnable {
        override fun run() {
            show()
            handler.postDelayed(this, 1000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // The pairing code is shown here; keep it out of screenshots and the recents preview.
        window.setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE)
        setContentView(R.layout.activity_main)
        status = findViewById(R.id.status)
        counters = findViewById(R.id.counters)
        toggle = findViewById(R.id.toggle)
        pair = findViewById(R.id.pair)
        pairing = findViewById(R.id.pairing)
        toggle.setOnClickListener {
            if (RelayService.relay == null) start() else stop()
            handler.postDelayed({ show() }, 200)
        }
        pair.setOnClickListener {
            RelayService.relay?.openPairing()
            pairing.update(getString(R.string.pairing_open))
        }
    }

    override fun onResume() {
        super.onResume()
        handler.post(refresh)
    }

    override fun onPause() {
        handler.removeCallbacksAndMessages(null)
        super.onPause()
    }

    private fun start() {
        // The relay runs without it; it only lets the notification, and its Stop button, show.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        }
        startForegroundService(Intent(this, RelayService::class.java))
    }

    private fun stop() {
        startService(Intent(this, RelayService::class.java).setAction(RelayService.ACTION_STOP))
    }

    private fun show() {
        val relay = RelayService.relay
        val (stats, code) = try {
            relay?.stats() to relay?.pairingCode()
        } catch (e: IllegalStateException) {
            null to null // stopped between the check and the call
        }
        pair.visibility = if (stats == null) View.GONE else View.VISIBLE
        if (code != null) {
            pairing.update(getString(R.string.pairing_code, code.chunked(3).joinToString(" ")))
        } else if (stats == null || pairing.text.toString() != getString(R.string.pairing_open)) {
            pairing.update("")
        }
        if (stats != null) RelayService.savePeers(this)
        if (stats == null) {
            status.update(
                RelayService.error?.let { getString(R.string.status_error, it) }
                    ?: getString(R.string.status_stopped),
            )
            counters.update("")
            toggle.update(getString(R.string.start))
        } else {
            val computers = stats.tunnels.toInt()
            status.update(resources.getQuantityString(R.plurals.status_running, computers, computers))
            counters.update(
                getString(
                    R.string.counters,
                    Formatter.formatFileSize(this, stats.bytesOut.toLong()),
                    Formatter.formatFileSize(this, stats.bytesIn.toLong()),
                ),
            )
            toggle.update(getString(R.string.stop))
        }
    }

    /** Sets text only when it changed, so the live region is not announced every second. */
    private fun TextView.update(value: String) {
        if (text.toString() != value) text = value
    }
}
