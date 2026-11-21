package io.github.heidaridev.kite

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.graphics.Paint
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
import android.widget.Switch
import android.widget.TextView
import io.github.heidaridev.kite.ffi.Relay
import io.github.heidaridev.kite.ffi.Stats
import io.github.heidaridev.kite.ffi.fingerprint

class MainActivity : Activity() {

    private val handler = Handler(Looper.getMainLooper())
    private lateinit var kite: KiteView
    private lateinit var status: TextView
    private lateinit var detail: TextView
    private lateinit var speed: TextView
    private lateinit var toggle: Button
    private lateinit var pair: Button
    private lateinit var steps: View
    private lateinit var detailsToggle: Button
    private lateinit var errorDetails: TextView
    private lateinit var pairing: TextView
    private lateinit var codeView: TextView
    private lateinit var wifi: Switch
    private lateinit var wifiLine: TextView
    private lateinit var computersTitle: TextView
    private lateinit var computers: LinearLayout
    private var listed: List<ByteArray> = emptyList()
    private var asking: AlertDialog? = null
    private var asked: String? = null
    private var battery: AlertDialog? = null
    private var removing: AlertDialog? = null
    private var showDetails = false
    private var openedAt = 0L
    private var sawCode = false

    // Throughput, from the byte counters of the last two looks; bytes per second.
    private var lastAt = 0L
    private var lastIn = 0L
    private var lastOut = 0L
    private var down = 0L
    private var up = 0L

    private val refresh = object : Runnable {
        override fun run() {
            show()
            handler.postDelayed(this, 1000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // The pairing code is shown here; keep it out of screenshots and the recents preview. Only
        // a debuggable build started with this extra (scripts/android-screens.sh) skips it, because
        // screencap shows a secure window black.
        val screenshots = applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0 &&
            intent.getBooleanExtra("screenshots", false)
        if (!screenshots) {
            window.setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE)
        }
        setContentView(R.layout.activity_main)
        kite = findViewById(R.id.kite)
        status = findViewById(R.id.status)
        detail = findViewById(R.id.detail)
        speed = findViewById(R.id.speed)
        toggle = findViewById(R.id.toggle)
        pair = findViewById(R.id.pair)
        steps = findViewById(R.id.steps)
        detailsToggle = findViewById(R.id.details_toggle)
        errorDetails = findViewById(R.id.error_details)
        pairing = findViewById(R.id.pairing)
        codeView = findViewById(R.id.code)
        wifi = findViewById(R.id.wifi)
        wifiLine = findViewById(R.id.wifi_line)
        computersTitle = findViewById(R.id.computers_title)
        computers = findViewById(R.id.computers)
        fitToScreen()
        wifi.isChecked = WifiShare.enabled(this)
        wifi.setOnCheckedChangeListener { _, on ->
            WifiShare.setEnabled(this, on)
            RelayService.syncWifi(this)
            handler.postDelayed({ show() }, 200)
        }
        toggle.setOnClickListener {
            if (RelayService.serving) stop() else start()
            handler.postDelayed({ show() }, 200)
        }
        detailsToggle.setOnClickListener {
            showDetails = !showDetails
            show()
        }
        findViewById<View>(R.id.step2).findViewById<TextView>(R.id.step_link).apply {
            text = getString(R.string.developer_options)
            paintFlags = paintFlags or Paint.UNDERLINE_TEXT_FLAG
            setOnClickListener { openDeveloperOptions() }
        }
        pair.setOnClickListener {
            try {
                RelayService.relay?.openPairing()
                openedAt = SystemClock.elapsedRealtime()
                show()
            } catch (e: IllegalStateException) {
                // stopped just now
            }
        }
        RelayService.load(this)
    }

    /** One column of at most 480dp, with the kite as large as the width and the height allow. */
    private fun fitToScreen() {
        val metrics = resources.displayMetrics
        val dp = metrics.density
        val column = minOf(metrics.widthPixels, (480 * dp).toInt())
        findViewById<View>(R.id.content).layoutParams.width = column
        findViewById<View>(R.id.bar).layoutParams.width = column
        // The steps and the buttons need the room; a small phone gets a small kite.
        val side = minOf(column - (48 * dp).toInt(), (metrics.heightPixels * 0.26f).toInt(), (200 * dp).toInt())
        kite.layoutParams.width = side
        kite.layoutParams.height = side
    }

    override fun onStart() {
        super.onStart()
        kite.resume()
    }

    override fun onStop() {
        kite.pause()
        super.onStop()
    }

    override fun onResume() {
        super.onResume()
        lastAt = 0L // the time since the last look is not a measuring window
        handler.post(refresh)
    }

    override fun onPause() {
        handler.removeCallbacksAndMessages(null)
        // The request stays pending on the phone; it is asked again when the app is back.
        asking?.dismiss()
        battery?.dismiss()
        removing?.dismiss()
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

    private fun openDeveloperOptions() {
        try {
            startActivity(Intent(Settings.ACTION_APPLICATION_DEVELOPMENT_SETTINGS))
        } catch (e: ActivityNotFoundException) {
            // Hidden until Developer options is turned on, and missing on some builds.
            try {
                startActivity(Intent(Settings.ACTION_SETTINGS))
            } catch (_: ActivityNotFoundException) {
                // No settings screen to open.
            }
        }
    }

    /** Removing a computer means pairing it again, so the user is asked first. */
    private fun confirmRemove(key: ByteArray, name: String) {
        removing?.dismiss()
        val dialog = AlertDialog.Builder(this)
            .setTitle(getString(R.string.remove_title, name))
            .setMessage(R.string.remove_message)
            .setPositiveButton(R.string.remove) { _, _ -> RelayService.removePeer(this, key) }
            .setNegativeButton(R.string.cancel, null)
            .setOnDismissListener { removing = null }
            .create()
        // A dialog is its own window, so the activity's FLAG_SECURE does not cover it.
        dialog.window?.setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE)
        dialog.show()
        removing = dialog
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
            .create()
        // A dialog is its own window, so the activity's FLAG_SECURE does not cover it.
        dialog.window?.setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE)
        dialog.show()
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
        measure(stats)

        val tunnels = stats?.tunnels?.toInt() ?: 0
        val error = if (stats == null) RelayService.error else null
        val serving = RelayService.serving
        kite.setTarget(
            when {
                error != null -> KiteView.State.ERROR
                stats == null -> if (serving) KiteView.State.CONNECTING else KiteView.State.DISCONNECTED
                tunnels > 0 -> KiteView.State.CONNECTED
                else -> KiteView.State.CONNECTING
            },
        )
        status.update(
            when {
                error != null -> getString(R.string.status_error)
                stats == null -> getString(if (serving) R.string.status_starting else R.string.status_stopped)
                tunnels > 0 -> resources.getQuantityString(R.plurals.status_running, tunnels, tunnels)
                else -> getString(R.string.status_waiting)
            },
        )
        // The words of the failure are for a bug report: one plain sentence first, the rest on request.
        if (error == null) showDetails = false
        detail.update(
            when {
                error != null -> getString(R.string.error_plain)
                stats == null -> if (serving) "" else getString(R.string.hint_stopped)
                else -> ""
            },
        )
        detailsToggle.update(if (error == null) "" else getString(if (showDetails) R.string.hide_details else R.string.show_details))
        errorDetails.update(if (showDetails) error ?: "" else "")

        speed.update(if (tunnels > 0) getString(R.string.speeds, rate(down), rate(up)) else "")
        toggle.update(getString(if (error != null) R.string.try_again else if (serving) R.string.stop else R.string.start))
        // Filled is the next step; stopping is not one, so it is an outline.
        toggle.look(filled = !serving)
        pair.look(filled = RelayService.paired.isEmpty())
        pair.visibility = if (stats == null) View.GONE else View.VISIBLE

        // The hint lasts as long as the relay keeps pairing open, or until the code has come and gone.
        if (code != null) sawCode = true else if (sawCode) { sawCode = false; openedAt = 0L }
        if (stats == null) openedAt = 0L
        val open = openedAt != 0L && SystemClock.elapsedRealtime() - openedAt <= PAIRING_OPEN_MS
        pairing.update(
            when {
                code != null -> getString(R.string.pairing_type)
                open -> getString(R.string.pairing_open)
                else -> ""
            },
        )
        // While a pairing is open its hint and code are the point; the steps make room.
        val guide = error == null && tunnels == 0 && code == null && !open
        steps.visibility = if (guide) View.VISIBLE else View.GONE
        if (guide) showSteps(serving, RelayService.paired.isNotEmpty())
        val wifiError = WifiShare.error
        val wifiUp = WifiShare.listening
        wifiLine.update(
            when {
                !wifi.isChecked -> ""
                stats == null -> getString(R.string.wifi_after_start)
                !WifiShare.synced -> ""
                wifiError != null -> getString(R.string.wifi_error, wifiError)
                wifiUp.isEmpty() -> getString(R.string.wifi_no_network)
                else -> getString(R.string.wifi_listening, wifiUp.joinToString(", "))
            },
        )
        codeView.update(code?.chunked(3)?.joinToString(" ") ?: "")
        // A screen reader would read "123 456" as two numbers.
        codeView.contentDescription = code?.toList()?.joinToString(" ")
    }

    /** The first-run card: the step to do now is marked, a finished one has a tick. */
    private fun showSteps(serving: Boolean, hasComputer: Boolean) {
        val now = if (serving) 1 else 0
        step(R.id.step1, 0, now, serving, R.string.step_start, getString(R.string.step_start_text))
        step(R.id.step2, 1, now, false, R.string.step_plug, getString(R.string.step_plug_text))
        step(
            R.id.step3, 2, now, false, R.string.step_open,
            getString(if (hasComputer) R.string.step_open_paired else R.string.step_open_text),
        )
        // Only the step to do now has the link: it is of no use before the phone is sharing.
        findViewById<View>(R.id.step2).findViewById<TextView>(R.id.step_link).visibility =
            if (now == 1) View.VISIBLE else View.GONE
    }

    private fun step(id: Int, index: Int, now: Int, done: Boolean, title: Int, text: String) {
        val row = findViewById<View>(id)
        val current = index == now
        val name = getString(title)
        row.findViewById<TextView>(R.id.step_title).update(name)
        row.findViewById<TextView>(R.id.step_text).update(text)
        row.findViewById<TextView>(R.id.marker).apply {
            update(if (done) "\u2713" else (index + 1).toString())
            setBackgroundResource(if (done) R.drawable.bg_marker_done else if (current) R.drawable.bg_marker_current else R.drawable.bg_marker)
            setTextColor(getColor(if (done) R.color.kite_bg else R.color.kite_text))
        }
        row.setBackgroundResource(if (current) R.drawable.bg_step_current else R.drawable.bg_step)
        val description = getString(R.string.step_description, index + 1, name, text) +
            (if (done) " " + getString(R.string.step_done) else if (current) " " + getString(R.string.step_current) else "")
        if (row.contentDescription?.toString() != description) row.contentDescription = description
    }

    /** Filled for the next step, an outline otherwise; set only when it changes, which keeps the ripple. */
    private fun Button.look(filled: Boolean) {
        if (tag == filled) return
        tag = filled
        setBackgroundResource(if (filled) R.drawable.bg_button_primary else R.drawable.bg_button_outline)
        setTextColor(getColor(if (filled) R.color.kite_on_button else R.color.kite_button))
    }

    /** Bytes per second since the last look, from the relay's counters. */
    private fun measure(stats: Stats?) {
        if (stats == null) {
            lastAt = 0L
            down = 0L
            up = 0L
            kite.setThroughput(0f)
            return
        }
        val now = SystemClock.elapsedRealtime()
        // A look a moment after the last one (a button press) would divide a few packets by a tiny time.
        if (lastAt != 0L && now - lastAt < MEASURE_MS) return
        val bytesIn = stats.bytesIn.toLong()
        val bytesOut = stats.bytesOut.toLong()
        if (lastAt != 0L && now > lastAt) {
            // The computer downloads what the relay sends it.
            down = ((bytesOut - lastOut) * 1000 / (now - lastAt)).coerceAtLeast(0L)
            up = ((bytesIn - lastIn) * 1000 / (now - lastAt)).coerceAtLeast(0L)
        }
        lastAt = now
        lastIn = bytesIn
        lastOut = bytesOut
        kite.setThroughput((down + up) * 8f / 1_000_000f)
    }

    private fun rate(bytesPerSecond: Long) =
        getString(R.string.speed, Formatter.formatFileSize(this, bytesPerSecond))

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
                setOnClickListener { confirmRemove(key, name) }
            }
            computers.addView(row)
        }
    }

    /** Sets text only when it changed, so a live region is not announced every second; empty hides it. */
    private fun TextView.update(value: String) {
        if (text.toString() != value) text = value
        visibility = if (value.isEmpty()) View.GONE else View.VISIBLE
    }

    private companion object {
        const val BATTERY_ASKED = "battery_asked"

        /** How long the relay keeps pairing open; the hint goes away with it. */
        const val PAIRING_OPEN_MS = 120_000L

        /** The shortest window the speed is measured over; motion.md samples at most twice a second. */
        const val MEASURE_MS = 900L
    }
}
