package io.github.heidaridev.kite

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import io.github.heidaridev.kite.ffi.Relay
import io.github.heidaridev.kite.ffi.RelayException
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

    /** False from the switch being turned until the relay has answered, so the screen shows no stale line. */
    @Volatile
    var synced = false
        private set

    // The two below belong to the service's worker thread.
    private var wanted: List<String>? = null
    private var advert: Advert? = null

    // Read from the main thread every second; the file is read once.
    @Volatile
    private var cached: Boolean? = null

    fun enabled(context: Context): Boolean =
        cached ?: context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(KEY, false)
            .also { cached = it }

    fun setEnabled(context: Context, on: Boolean) {
        cached = on
        synced = false
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
            synced = false
        } else if (want == wanted && error == null) {
            return
        } else {
            wanted = want
        }
        try {
            val up = relay?.shareWifi(want) ?: emptyList()
            listening = up
            error = null
            // An address that did not bind (a hotspot not ready yet) is tried again at the next look.
            if (up.size < want.size) wanted = null
            if (up.isEmpty()) {
                advert?.close()
                advert = null
            } else if (advert == null) {
                advert = Advert(context, up.first().substringAfterLast(':').toInt())
            }
        } catch (e: Exception) {
            // The relay says why: a taken port, or it was stopped in the meantime.
            listening = emptyList()
            error = (e as? RelayException.Io)?.reason ?: e.message ?: e.javaClass.simpleName
            advert?.close()
            advert = null
        }
        synced = relay != null
    }

    /**
     * The private IPv4 addresses of interfaces that are up and are WiFi, ethernet, hotspot or
     * tethering ones, and not the phone's mobile data or a VPN. It is an allow-list: an interface
     * counts when the system lists it as a WiFi or ethernet network, or when its name is a usual
     * hotspot or tethering one (a hotspot has no network of its own in ConnectivityManager), and
     * the system does not list it as cellular or VPN. Anything unknown is left out. The relay
     * checks the addresses again.
     */
    private fun addresses(context: Context): List<String> {
        val connectivity = context.getSystemService(ConnectivityManager::class.java)
        val allow = HashSet<String>()
        val skip = HashSet<String>()
        @Suppress("DEPRECATION") // there is no other list of every network
        for (network in connectivity.allNetworks) {
            val caps = connectivity.getNetworkCapabilities(network) ?: continue
            val name = connectivity.getLinkProperties(network)?.interfaceName ?: continue
            if (caps.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) ||
                caps.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
            ) {
                skip.add(name)
            } else if (caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) ||
                caps.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) ||
                caps.hasTransport(NetworkCapabilities.TRANSPORT_WIFI_AWARE)
            ) {
                allow.add(name)
            }
        }
        val found = ArrayList<String>()
        try {
            for (nic in NetworkInterface.getNetworkInterfaces()?.asSequence().orEmpty()) {
                val name = nic.name
                if (!nic.isUp || nic.isLoopback || nic.isPointToPoint || name in skip) continue
                if (name !in allow && HOTSPOT_PREFIXES.none { name.startsWith(it) }) continue
                for (address in nic.inetAddresses) {
                    if (address is Inet4Address && address.isSiteLocalAddress) address.hostAddress?.let(found::add)
                }
            }
        } catch (e: java.net.SocketException) {
            return emptyList() // no list now; the next look tries again
        }
        return found.distinct().sorted()
    }

    /** Names of the interfaces a hotspot, WiFi Direct and USB or ethernet tethering use. */
    private val HOTSPOT_PREFIXES = listOf("ap", "swlan", "wlan", "softap", "p2p", "rndis", "ncm", "eth")

    /**
     * `_kite._tcp` with a version in the TXT record and nothing else: a key or name there would
     * let anyone who listens on the network follow the phone from one network to the next.
     * Best effort: a computer can still be given the address by hand.
     */
    private class Advert(context: Context, port: Int) : NsdManager.RegistrationListener {
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
