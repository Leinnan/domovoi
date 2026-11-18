package com.crayen.domovoi.core

import com.crayen.domovoi.app.HttpError
import com.crayen.domovoi.app.HttpHeader
import com.crayen.domovoi.app.HttpRequest
import com.crayen.domovoi.app.HttpResult
import com.novi.serde.Bytes
import kotlinx.coroutines.runBlocking
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

class HttpTest {
    private val server = MockWebServer()

    @Before fun start() = server.start()
    @After fun stop() = server.close()

    private fun request(method: String, body: String = "", headers: List<HttpHeader> = emptyList()) =
        HttpRequest(method, server.url("/api/x").toString(), headers, Bytes(body.toByteArray()))

    @Test fun anyStatusIsAnAnswer() = runBlocking {
        server.enqueue(MockResponse.Builder().code(401).body("""{"error":"no"}""").build())
        val result = Http().perform(request("GET"))
        assertTrue(result is HttpResult.Ok)
        val response = (result as HttpResult.Ok).value
        assertEquals(401.toUShort(), response.status)
        assertEquals("""{"error":"no"}""", String(response.body.content))
    }

    @Test fun aBodyDefaultsToJsonAndKeepsHeaders() = runBlocking {
        server.enqueue(MockResponse.Builder().code(200).build())
        Http().perform(request("POST", "{}", listOf(HttpHeader("Authorization", "Bearer t"))))
        val recorded = server.takeRequest()
        assertEquals("POST", recorded.method)
        assertEquals("Bearer t", recorded.headers["Authorization"])
        assertTrue(recorded.headers["Content-Type"]!!.startsWith("application/json"))
        assertEquals("{}", recorded.body?.utf8())
    }

    @Test fun anExplicitContentTypeWins() = runBlocking {
        server.enqueue(MockResponse.Builder().code(200).build())
        Http().perform(request("POST", "a=b", listOf(HttpHeader("Content-Type", "application/x-www-form-urlencoded"))))
        assertTrue(server.takeRequest().headers["Content-Type"]!!.startsWith("application/x-www-form-urlencoded"))
    }

    @Test fun anEmptyPostStillSends() = runBlocking {
        server.enqueue(MockResponse.Builder().code(204).build())
        val result = Http().perform(request("POST"))
        assertEquals(204.toUShort(), (result as HttpResult.Ok).value.status)
    }

    @Test fun aBadUrlIsAUrlError() = runBlocking {
        val result = Http().perform(HttpRequest("GET", "not a url", emptyList(), Bytes(ByteArray(0))))
        assertTrue((result as HttpResult.Err).value is HttpError.Url)
    }

    @Test fun noAnswerIsAnIoError() = runBlocking {
        val url = server.url("/").toString()
        server.close()
        val result = Http().perform(HttpRequest("GET", url, emptyList(), Bytes(ByteArray(0))))
        assertTrue((result as HttpResult.Err).value is HttpError.Io)
    }
}
