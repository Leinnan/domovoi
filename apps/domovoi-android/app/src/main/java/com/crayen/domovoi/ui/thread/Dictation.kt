package com.crayen.domovoi.ui.thread

import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Speak instead of typing (`Dictation.swift`): Android's recognizer, on the
 * device when it can be. `onText` gets the transcript so far, each time it grows.
 */
class Dictation(private val context: Context) {
    var listening by mutableStateOf(false)
        private set
    var error by mutableStateOf<String?>(null)
        private set
    private var recognizer: SpeechRecognizer? = null
    private var finished = ""

    fun toggle(onText: (String) -> Unit) = if (listening) stop() else start(onText)

    private fun start(onText: (String) -> Unit) {
        error = null
        if (!SpeechRecognizer.isRecognitionAvailable(context)) {
            error = "Speech recognition isn't available on this device."
            return
        }
        val onDevice = SpeechRecognizer.isOnDeviceRecognitionAvailable(context)
        val r = if (onDevice) SpeechRecognizer.createOnDeviceSpeechRecognizer(context) else SpeechRecognizer.createSpeechRecognizer(context)
        recognizer = r
        finished = ""
        r.setRecognitionListener(object : RecognitionListener {
            override fun onPartialResults(partial: Bundle) {
                partial.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()?.let { onText(join(finished, it)) }
            }
            override fun onResults(results: Bundle) {
                results.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull()?.let { finished = join(finished, it); onText(finished) }
                stop()
            }
            override fun onError(code: Int) {
                if (code != SpeechRecognizer.ERROR_NO_MATCH && code != SpeechRecognizer.ERROR_SPEECH_TIMEOUT && code != SpeechRecognizer.ERROR_CLIENT) {
                    error = if (code == SpeechRecognizer.ERROR_INSUFFICIENT_PERMISSIONS) "Domovoi may not use the microphone." else "Dictation stopped (error $code)."
                }
                stop()
            }
            override fun onReadyForSpeech(params: Bundle?) {}
            override fun onBeginningOfSpeech() {}
            override fun onRmsChanged(rmsdB: Float) {}
            override fun onBufferReceived(buffer: ByteArray?) {}
            override fun onEndOfSpeech() {}
            override fun onEvent(eventType: Int, params: Bundle?) {}
        })
        val intent = Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH)
            .putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
            .putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, true)
            .putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
        r.startListening(intent)
        listening = true
    }

    fun stop() {
        listening = false
        recognizer?.let { it.stopListening(); it.destroy() }
        recognizer = null
    }

    private fun join(a: String, b: String) = if (a.isEmpty()) b else "$a $b"
}
