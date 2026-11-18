package com.crayen.domovoi.core

import com.crayen.domovoi.app.HttpError
import com.crayen.domovoi.app.HttpHeader
import com.crayen.domovoi.app.HttpRequest
import com.crayen.domovoi.app.HttpResponse
import com.crayen.domovoi.app.HttpResult
import com.novi.serde.Bytes
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.MediaType.Companion.toMediaTypeOrNull
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.internal.http.HttpMethod
import java.util.concurrent.TimeUnit

/**
 * Runs the core's HTTP requests. The core adds the bearer token and judges
 * the status itself, so every response (4xx and 5xx too) resolves as `Ok`;
 * only a request that never got an answer is an error. No cookies or cache:
 * one loopback address can front different remote servers over time.
 */
class Http(private val client: OkHttpClient = defaultClient()) {
    suspend fun perform(request: HttpRequest): HttpResult = withContext(Dispatchers.IO) {
        val url = request.url.toHttpUrlOrNull() ?: return@withContext HttpResult.Err(HttpError.Url("Invalid URL"))
        val builder = Request.Builder().url(url)
        var contentType: String? = null
        for (header in request.headers) {
            if (header.name.equals("Content-Type", ignoreCase = true)) contentType = header.value
            else builder.addHeader(header.name, header.value)
        }
        val bytes = request.body.content
        val method = request.method.uppercase()
        val body = when {
            bytes.isNotEmpty() -> bytes.toRequestBody((contentType ?: "application/json").toMediaTypeOrNull())
            HttpMethod.requiresRequestBody(method) -> ByteArray(0).toRequestBody(contentType?.toMediaTypeOrNull())
            else -> null
        }
        builder.method(method, body)
        try {
            client.newCall(builder.build()).execute().use { response ->
                val headers = response.headers.map { (name, value) -> HttpHeader(name, value) }
                HttpResult.Ok(HttpResponse(response.code.toUShort(), headers, Bytes(response.body.bytes())))
            }
        } catch (e: Exception) {
            HttpResult.Err(HttpError.Io(e.message ?: e.javaClass.simpleName))
        }
    }

    companion object {
        fun defaultClient(): OkHttpClient = OkHttpClient.Builder()
            .callTimeout(30, TimeUnit.SECONDS)
            .connectTimeout(30, TimeUnit.SECONDS)
            .readTimeout(30, TimeUnit.SECONDS)
            .retryOnConnectionFailure(false)
            .build()
    }
}
