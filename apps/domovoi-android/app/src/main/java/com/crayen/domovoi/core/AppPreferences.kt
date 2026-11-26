package com.crayen.domovoi.core

import android.content.Context
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Client-local theme override, saved on this device only. */
enum class AppAppearance(val key: String, val label: String) {
    System("system", "System"),
    Light("light", "Light"),
    Dark("dark", "Dark");

    companion object {
        fun fromKey(key: String?): AppAppearance = entries.firstOrNull { it.key == key } ?: System
    }
}

/** Device-local app preferences stored in SharedPreferences (never sent to any server). */
object AppPreferences {
    private const val PREFS = "domovoi.prefs"
    private const val KEY_APPEARANCE = "appearance"
    private const val KEY_DYNAMIC_COLOR = "dynamic_color"

    private val _appearance = MutableStateFlow(AppAppearance.System)
    val appearance: StateFlow<AppAppearance> = _appearance.asStateFlow()

    private val _dynamicColor = MutableStateFlow(true)
    val dynamicColor: StateFlow<Boolean> = _dynamicColor.asStateFlow()

    private var loaded = false

    fun init(context: Context) {
        if (loaded) return
        loaded = true
        val prefs = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        _appearance.value = AppAppearance.fromKey(prefs.getString(KEY_APPEARANCE, null))
        _dynamicColor.value = prefs.getBoolean(KEY_DYNAMIC_COLOR, true)
    }

    fun setAppearance(context: Context, value: AppAppearance) {
        _appearance.value = value
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putString(KEY_APPEARANCE, value.key).apply()
    }

    fun setDynamicColor(context: Context, enabled: Boolean) {
        _dynamicColor.value = enabled
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            .edit().putBoolean(KEY_DYNAMIC_COLOR, enabled).apply()
    }
}
