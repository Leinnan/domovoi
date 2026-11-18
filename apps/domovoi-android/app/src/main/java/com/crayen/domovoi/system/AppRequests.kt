package com.crayen.domovoi.system

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Something from outside the app (the share sheet, a shortcut) that the UI acts on once it can. */
sealed interface AppRequest {
    /** A new thread, its draft prefilled: shared text and images (JPEG data URLs). */
    data class NewThread(val text: String = "", val images: List<com.crayen.domovoi.ui.thread.PreparedImage> = emptyList()) : AppRequest
    /** A notification or link opened this thread: show it even if it is already selected. */
    data class OpenThread(val id: String) : AppRequest
}

object AppRequests {
    private val _pending = MutableStateFlow<AppRequest?>(null)
    val pending: StateFlow<AppRequest?> = _pending.asStateFlow()

    fun post(request: AppRequest) { _pending.value = request }

    /** The request, once: the UI takes it when it can act on it. */
    fun take(): AppRequest? = _pending.value.also { _pending.value = null }
}
