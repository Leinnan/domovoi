package com.crayen.domovoi.core

import com.crayen.domovoi.app.TunnelOutput
import com.crayen.domovoi.ffi.TunnelFfi
import kotlinx.coroutines.asCoroutineDispatcher
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.util.concurrent.Executors

/**
 * The iroh bridge (`TunnelFfi`). Its calls block (dialing takes seconds), so
 * they run on one background thread: start, status and stop never overlap,
 * and a stop queued behind a start stops what that start made.
 */
class Tunnel(private val relays: String = "") {
    private val thread = Executors.newSingleThreadExecutor { Thread(it, "domovoi-tunnel") }.asCoroutineDispatcher()
    private var ffi: TunnelFfi? = null

    private fun bridge(): TunnelFfi = ffi ?: TunnelFfi().also { ffi = it }

    /** The loopback port, or the reason it failed. */
    suspend fun start(key: String, ticket: String): Result<UShort> = withContext(thread) {
        try {
            Result.success(bridge().start(key, ticket, relays))
        } catch (e: Exception) {
            Result.failure(e)
        }
    }

    /** The bridge's state, or null when it isn't running. */
    suspend fun status(): TunnelOutput.Status? = withContext(thread) {
        val json = try { bridge().status() } catch (e: Exception) { return@withContext null }
        parseStatus(json)
    }

    suspend fun networkChanged() = withContext(thread) { runCatching { bridge().networkChange() } }

    suspend fun stop() = withContext(thread) { runCatching { bridge().stop() } }

    companion object {
        fun parseStatus(json: String): TunnelOutput.Status? {
            if (json.isBlank() || json == "null") return null
            val o = runCatching { JSONObject(json) }.getOrNull() ?: return null
            val rtt = if (o.isNull("rttMs")) 0L else o.optLong("rttMs", 0)
            return TunnelOutput.Status(
                connected = o.optBoolean("connected", false),
                path = if (o.isNull("path")) "" else o.optString("path", ""),
                rttMs = rtt.coerceIn(0, 60_000).toUInt(),
                error = if (o.isNull("error")) "" else o.optString("error", ""),
                listening = if (o.isNull("listening")) true else o.optBoolean("listening", true),
            )
        }
    }
}
