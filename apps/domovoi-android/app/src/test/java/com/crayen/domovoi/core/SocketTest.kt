package com.crayen.domovoi.core

import com.crayen.domovoi.app.SocketEvent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.asCoroutineDispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import java.util.concurrent.Executors
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

class SocketTest {
    private val server = MockWebServer()
    private val events = LinkedBlockingQueue<Pair<SocketEvent, UInt>>()
    private val main = Executors.newSingleThreadExecutor().asCoroutineDispatcher()
    private lateinit var socket: Socket

    @Before fun start() {
        server.start()
        socket = Socket(CoroutineScope(main), { e, id -> events.add(e to id) })
    }

    @After fun stop() {
        server.close()
        main.close()
    }

    private fun next() = events.poll(5, TimeUnit.SECONDS)
    private fun wsUrl() = server.url("/ws").toString().replace("http", "ws")

    /** The server side: says hello on open, echoes, closes on "bye". */
    private fun serve() = server.enqueue(MockResponse.Builder().webSocketUpgrade(object : WebSocketListener() {
        override fun onOpen(webSocket: WebSocket, response: Response) { webSocket.send("hello") }
        override fun onMessage(webSocket: WebSocket, text: String) {
            if (text == "bye") webSocket.close(1000, "done") else webSocket.send("echo:$text")
        }
        override fun onClosing(webSocket: WebSocket, code: Int, reason: String) { webSocket.close(1000, null) }
    }).build())

    @Test fun opensStreamsAndCloses() {
        serve()
        socket.open(wsUrl(), 7u)
        assertEquals(SocketEvent.Opened to 7u, next())
        assertEquals(SocketEvent.Message("hello") to 7u, next())
        socket.send("ping")
        assertEquals(SocketEvent.Message("echo:ping") to 7u, next())
        socket.send("bye")
        assertEquals(SocketEvent.Closed("done") to 7u, next())
        assertNull(events.poll(500, TimeUnit.MILLISECONDS))
    }

    @Test fun closingSaysNothing() {
        serve()
        socket.open(wsUrl(), 1u)
        assertEquals(SocketEvent.Opened to 1u, next())
        next() // hello
        socket.close()
        assertNull(events.poll(1, TimeUnit.SECONDS))
    }

    @Test fun aReplacedSocketGoesQuiet() {
        serve()
        serve()
        socket.open(wsUrl(), 1u)
        assertEquals(SocketEvent.Opened to 1u, next())
        next() // hello on the first
        socket.open(wsUrl(), 2u)
        assertEquals(SocketEvent.Opened to 2u, next())
        assertEquals(SocketEvent.Message("hello") to 2u, next())
        assertNull(events.poll(1, TimeUnit.SECONDS))
    }

    @Test fun aRefusedUpgradeIsClosed() {
        server.enqueue(MockResponse.Builder().code(401).build())
        socket.open(wsUrl(), 3u)
        val (event, id) = next()
        assertEquals(3u, id)
        assertTrue(event is SocketEvent.Closed && event.reason.contains("401"))
    }

    @Test fun aBadUrlIsClosed() {
        socket.open("::", 4u)
        assertEquals(SocketEvent.Closed("Invalid server URL") to 4u, next())
    }
}
