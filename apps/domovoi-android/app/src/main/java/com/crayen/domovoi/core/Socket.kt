package com.crayen.domovoi.core

import com.crayen.domovoi.app.SocketEvent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import java.util.concurrent.TimeUnit

/**
 * The core's WebSocket. `open` streams `Opened`, then `Message`s, then one
 * `Closed`. `send` and `close` are fire-and-forget. A socket that was closed
 * or replaced never reports again (the core also drops stale events by
 * attempt). Every callback hops to `scope` (the core's thread) before it
 * touches state or emits.
 *
 * OkHttp pings every 20 s and fails a socket whose pong doesn't come back,
 * so a connection that died without a word (Wi-Fi gone, a NAT that forgot
 * it) is noticed in seconds.
 */
class Socket(
    private val scope: CoroutineScope,
    private val emit: (SocketEvent, UInt) -> Unit,
    baseClient: OkHttpClient = OkHttpClient(),
    private val handshakeTimeoutMs: Long = 20_000,
    pingIntervalMs: Long = 20_000,
) {
    private val client = baseClient.newBuilder()
        .pingInterval(pingIntervalMs, TimeUnit.MILLISECONDS)
        .connectTimeout(handshakeTimeoutMs, TimeUnit.MILLISECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .build()

    private var current: WebSocket? = null
    private var opening: WebSocket? = null
    private var watchdog: Job? = null

    fun open(url: String, requestId: UInt) {
        // Forget the old socket before cancelling it: its failure callback
        // must find it replaced, not current, and stay quiet.
        val old = current
        current = null
        opening = null
        old?.cancel()
        watchdog?.cancel()
        val request = try {
            Request.Builder().url(url).build()
        } catch (e: IllegalArgumentException) {
            emit(SocketEvent.Closed("Invalid server URL"), requestId)
            return
        }
        lateinit var socket: WebSocket
        socket = client.newWebSocket(request, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) = onMain(webSocket) {
                if (opening !== webSocket) return@onMain
                opening = null
                watchdog?.cancel()
                emit(SocketEvent.Opened, requestId)
            }

            override fun onMessage(webSocket: WebSocket, text: String) = onMain(webSocket) {
                emit(SocketEvent.Message(text), requestId)
            }

            override fun onMessage(webSocket: WebSocket, bytes: ByteString) = onMain(webSocket) {
                emit(SocketEvent.Message(bytes.utf8()), requestId)
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) = onMain(webSocket) {
                drop(webSocket)
                webSocket.close(1000, null)
                emit(SocketEvent.Closed(reason.ifEmpty { "The server closed the connection." }), requestId)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) = onMain(webSocket) {
                drop(webSocket)
                emit(SocketEvent.Closed(reason.ifEmpty { "The server closed the connection." }), requestId)
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) = onMain(webSocket) {
                val wasOpening = opening === webSocket
                drop(webSocket)
                emit(SocketEvent.Closed(describe(t, response, wasOpening)), requestId)
            }
        })
        current = socket
        opening = socket
        // A handshake that never completes (a server that stopped answering).
        watchdog = scope.launch {
            delay(handshakeTimeoutMs)
            if (current === socket && opening === socket) {
                drop(socket)
                socket.cancel()
                emit(SocketEvent.Closed("The server did not answer."), requestId)
            }
        }
    }

    fun send(text: String) {
        current?.send(text)
    }

    fun close() {
        val socket = current
        current = null
        opening = null
        watchdog?.cancel()
        socket?.close(1000, null)
    }

    private fun drop(socket: WebSocket) {
        if (current === socket) current = null
        if (opening === socket) opening = null
    }

    /** Runs `block` on `scope` if `socket` is still the current one. */
    private fun onMain(socket: WebSocket, block: () -> Unit) {
        scope.launch { if (current === socket) block() }
    }

    private fun describe(t: Throwable, response: Response?, opening: Boolean): String {
        val message = t.message.orEmpty()
        return when {
            response != null && opening -> "Connection failed: the server answered ${response.code}"
            message.contains("didn't receive pong", ignoreCase = true) -> "The connection stopped answering."
            opening -> "Connection failed: ${message.ifEmpty { t.javaClass.simpleName }}"
            else -> "The connection dropped: ${message.ifEmpty { t.javaClass.simpleName }}"
        }
    }
}
