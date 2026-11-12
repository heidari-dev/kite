package io.github.heidaridev.kite

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.provider.Settings
import android.text.format.Formatter
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import io.github.heidaridev.kite.ffi.Relay
import io.github.heidaridev.kite.ffi.fingerprint

class MainActivity : Activity() {

    private val handler = Handler(Looper.getMainLooper())
    private lateinit var status: TextView
    private lateinit var counters: TextView
    private lateinit var toggle: Button
    private lateinit var pair: Button
    private lateinit var pairing: TextView
    private lateinit var computersTitle: TextView
    private lateinit var computers: LinearLayout
    private var listed: List<ByteArray> = emptyList()
    private var asking: AlertDialog? = null
    private var asked: String? = null
    private var battery: AlertDialog? = null
    private var openedAt = 0L

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
        computersTitle = findViewById(R.id.computers_title)
        computers = findViewById(R.id.computers)
        toggle.setOnClickListener {
            if (RelayService.relay == null) start() else stop()
            handler.postDelayed({ show() }, 200)
        }
        pair.setOnClickListener {
            try {
                RelayService.relay?.openPairing()
                openedAt = SystemClock.elapsedRealtime()
                pairing.update(getString(R.string.pairing_open))
            } catch (e: IllegalStateException) {
                // stopped just now
            }
        }
        RelayService.load(this)
    }

    override fun onResume() {
        super.onResume()
        handler.post(refresh)
    }

    override fun onPause() {
        handler.removeCallbacksAndMessages(null)
        // The request stays pending on the phone; it is asked again when the app is back.
        asking?.dismiss()
        battery?.dismiss()
        super.onPause()
    }

    private fun start() {
        startForegroundService(Intent(this, RelayService::class.java))
        // The relay runs without it; it only lets the notification, and its Stop button, show.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 0)
        } else {
            askBatteryOptimisation()
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        askBatteryOptimisation()
    }

    private fun stop() {
        startService(Intent(this, RelayService::class.java).setAction(RelayService.ACTION_STOP))
    }

    /** Once: Android may stop a background app with the screen off, which cuts the computer's link. */
    private fun askBatteryOptimisation() {
        val prefs = getPreferences(MODE_PRIVATE)
        if (getSystemService(PowerManager::class.java).isIgnoringBatteryOptimizations(packageName) ||
            prefs.getBoolean(BATTERY_ASKED, false) || isFinishing || isDestroyed || battery != null
        ) {
            return
        }
        // Asked once: any answer ends the question, closing the app does not.
        val answered = { prefs.edit().putBoolean(BATTERY_ASKED, true).apply() }
        battery = AlertDialog.Builder(this)
            .setTitle(R.string.battery_title)
            .setMessage(R.string.battery_message)
            .setPositiveButton(R.string.allow) { _, _ ->
                answered()
                try {
                    startActivity(
                        Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")),
                    )
                } catch (e: ActivityNotFoundException) {
                    // Some builds have no such screen; sharing works without it.
                }
            }
            .setNegativeButton(R.string.not_now) { _, _ -> answered() }
            .setOnDismissListener { battery = null }
            .show()
    }

    /** A computer that reached the phone asks to pair; only the user's yes lets it go on. */
    private fun ask(relay: Relay, fingerprint: String) {
        asked = fingerprint
        val dialog = AlertDialog.Builder(this)
            .setTitle(R.string.allow_title)
            .setMessage(getString(R.string.allow_message, fingerprint))
            .setCancelable(false)
            .setPositiveButton(R.string.allow) { _, _ -> answer(relay, fingerprint, true) }
            .setNegativeButton(R.string.deny) { _, _ -> answer(relay, fingerprint, false) }
            .setOnDismissListener { asking = null }
            .show()
        // Another app drawing over the dialog must not be able to press Allow for the user.
        dialog.getButton(AlertDialog.BUTTON_POSITIVE).filterTouchesWhenObscured = true
        dialog.getButton(AlertDialog.BUTTON_NEGATIVE).filterTouchesWhenObscured = true
        asking = dialog
    }

    /** The answer names the computer the dialog showed; the relay ignores it if another waits now. */
    private fun answer(relay: Relay, fingerprint: String, allow: Boolean) {
        try {
            relay.answerPairing(fingerprint, allow)
        } catch (e: IllegalStateException) {
            // stopped while the dialog was open
        }
    }

    private fun show() {
        val relay = RelayService.relay
        val shown = try {
            relay?.let { Triple(it.stats(), it.pairingCode(), it.pairingRequest()) }
        } catch (e: IllegalStateException) {
            null // stopped between the check and the call
        }
        val stats = shown?.first
        val code = shown?.second
        val request = shown?.third
        // The request ended (answered elsewhere, expired, hung up) or is another computer now.
        if (request != asked) asking?.dismiss()
        if (relay != null && request != null && asking == null) ask(relay, request)
        showComputers(RelayService.paired)
        pair.visibility = if (stats == null) View.GONE else View.VISIBLE
        if (code != null) {
            pairing.update(getString(R.string.pairing_code, code.chunked(3).joinToString(" ")))
        } else if (stats == null || pairing.text.toString() != getString(R.string.pairing_open) ||
            SystemClock.elapsedRealtime() - openedAt > PAIRING_OPEN_MS
        ) {
            pairing.update("")
        }
        if (stats == null) {
            status.update(
                RelayService.error?.let { getString(R.string.status_error, it) }
                    ?: getString(R.string.status_stopped),
            )
            counters.update("")
            toggle.update(getString(R.string.start))
        } else {
            val tunnels = stats.tunnels.toInt()
            status.update(resources.getQuantityString(R.plurals.status_running, tunnels, tunnels))
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

    /** Rebuilds the list only when the paired computers changed. */
    private fun showComputers(keys: List<ByteArray>) {
        if (keys.size == listed.size && keys.zip(listed).all { (a, b) -> a.contentEquals(b) }) return
        listed = keys
        computers.removeAllViews()
        computersTitle.visibility = if (keys.isEmpty()) View.GONE else View.VISIBLE
        for (key in keys) {
            val name = fingerprint(key) ?: "?"
            val row = layoutInflater.inflate(R.layout.item_computer, computers, false)
            row.findViewById<TextView>(R.id.name).text = name
            row.findViewById<Button>(R.id.remove).apply {
                contentDescription = getString(R.string.remove_computer, name)
                setOnClickListener { RelayService.removePeer(this@MainActivity, key) }
            }
            computers.addView(row)
        }
    }

    /** Sets text only when it changed, so the live region is not announced every second. */
    private fun TextView.update(value: String) {
        if (text.toString() != value) text = value
    }

    private companion object {
        const val BATTERY_ASKED = "battery_asked"

        /** How long the relay keeps pairing open; the hint goes away with it. */
        const val PAIRING_OPEN_MS = 120_000L
    }
}
