package io.github.heidaridev.kite

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import io.github.heidaridev.kite.ffi.Relay
import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * Sharing over WiFi: lets the relay listen on the phone's addresses on WiFi, hotspot and ethernet
 * networks, never on mobile data or a VPN, and announces it as `_kite._tcp` while it does. It is
 * off until the user turns it on, and it only runs while the relay does.
 */
internal object WifiShare {

    private const val PREFS = "wifi"
    private const val KEY = "share"

    /** The `address:port` pairs the relay listens on now. */
    @Volatile
    var listening: List<String> = emptyList()
        private set

    /** Why the relay could not listen, or null. */
    @Volatile
    var error: String? = null
        private set

    // The two below belong to the service's worker thread.
    private var wanted: List<String>? = null
    private var advert: Advert? = null

    fun enabled(context: Context): Boolean =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY, false)

    fun setEnabled(context: Context, on: Boolean) {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(KEY, on).apply()
    }

    /**
     * Makes the relay listen on the current addresses, or on none when sharing is off or `relay` is
     * null. Cheap when nothing changed, so it can run every few seconds. Worker thread only.
     */
    fun sync(context: Context, relay: Relay?) {
        val want = if (relay != null && enabled(context)) addresses(context) else emptyList()
        if (relay == null) {
            wanted = null
        } else if (want == wanted && error == null) {
            return
        } else {
            wanted = want
        }
        try {
            val up = relay?.shareWifi(want) ?: emptyList()
            listening = up
            error = null
            if (up.isEmpty()) {
                advert?.close()
                advert = null
            } else if (advert == null) {
                advert = Advert(context, up.first().substringAfterLast(':').toInt(), relay?.ownFingerprint())
            }
        } catch (e: Exception) {
            // The relay says why: a taken port, or it was stopped in the meantime.
            listening = emptyList()
            error = e.message ?: e.javaClass.simpleName
            advert?.close()
            advert = null
        }
    }

    /**
     * The private IPv4 addresses of every interface that is up and is not the phone's mobile data
     * or a VPN. A hotspot has no network of its own in ConnectivityManager, so interfaces are
     * listed by what they are not. The relay checks the addresses again.
     */
    private fun addresses(context: Context): List<String> {
        val connectivity = context.getSystemService(ConnectivityManager::class.java)
        val skip = HashSet<String>()
        @Suppress("DEPRECATION") // there is no other list of every network
        for (network in connectivity.allNetworks) {
            val caps = connectivity.getNetworkCapabilities(network) ?: continue
            if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) ||
                caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
            ) {
                connectivity.getLinkProperties(network)?.interfaceName?.let(skip::add)
            }
        }
        val found = ArrayList<String>()
        try {
            for (nic in NetworkInterface.getNetworkInterfaces()?.asSequence().orEmpty()) {
                if (!nic.isUp || nic.isLoopback || nic.isPointToPoint || nic.name in skip) continue
                for (address in nic.inetAddresses) {
                    if (address is Inet4Address && address.isSiteLocalAddress) address.hostAddress?.let(found::add)
                }
            }
        } catch (e: java.net.SocketException) {
            return emptyList() // no list now; the next look tries again
        }
        return found.distinct().sorted()
    }

    /**
     * `_kite._tcp` with a version and the phone's key fingerprint, which anyone who connects sees
     * anyway. Best effort: a computer can still be given the address by hand.
     */
    private class Advert(context: Context, port: Int, fingerprint: String?) : NsdManager.RegistrationListener {
        private val nsd = context.getSystemService(NsdManager::class.java)

        @Volatile
        private var registered = false

        @Volatile
        private var closed = false

        init {
            val info = NsdServiceInfo().apply {
                serviceName = "Kite"
                serviceType = "_kite._tcp"
                setPort(port)
                setAttribute("v", "1")
                fingerprint?.let { setAttribute("fp", it.replace(" ", "")) }
            }
            try {
                nsd.registerService(info, NsdManager.PROTOCOL_DNS_SD, this)
            } catch (e: RuntimeException) {
                closed = true
            }
        }

        fun close() {
            closed = true
            if (registered) unregister()
        }

        private fun unregister() {
            try {
                registered = false
                nsd.unregisterService(this)
            } catch (e: IllegalArgumentException) {
                // not registered any more
            }
        }

        override fun onServiceRegistered(info: NsdServiceInfo) {
            registered = true
            // Closed before the system finished registering.
            if (closed) unregister()
        }

        override fun onRegistrationFailed(info: NsdServiceInfo, errorCode: Int) {}
        override fun onServiceUnregistered(info: NsdServiceInfo) {}
        override fun onUnregistrationFailed(info: NsdServiceInfo, errorCode: Int) {}
    }
}
