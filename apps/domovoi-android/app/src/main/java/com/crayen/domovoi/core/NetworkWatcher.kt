package com.crayen.domovoi.core

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Handler
import android.os.Looper

/**
 * Reports whether the device has a network (`online`, also the first
 * report) and when the network under the app changes (Wi-Fi to cellular),
 * on the main thread.
 */
class NetworkWatcher(context: Context, private val online: (Boolean) -> Unit, private val changed: () -> Unit) {
    private val manager = context.getSystemService(ConnectivityManager::class.java)
    private val main = Handler(Looper.getMainLooper())
    private var isOnline: Boolean? = null
    private var network: Network? = null

    fun start() {
        val active = manager.activeNetwork
        val caps = active?.let { manager.getNetworkCapabilities(it) }
        report(active, caps?.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) == true)
        manager.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(n: Network) = main.post { report(n, true) }.let { }
            override fun onLost(n: Network) = main.post { if (network == n) report(null, false) }.let { }
        })
    }

    private fun report(n: Network?, up: Boolean) {
        val first = isOnline == null
        if (isOnline != up) {
            isOnline = up
            online(up)
        }
        if (!first && up && n != network && network != null) changed()
        network = n
    }
}
