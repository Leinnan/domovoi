package com.crayen.domovoi

import android.app.Application
import android.content.Context
import androidx.lifecycle.DefaultLifecycleObserver
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.ProcessLifecycleOwner
import com.crayen.domovoi.core.Core
import com.crayen.domovoi.system.SystemSurfaces

/** Owns the one `Core` of the process and tells it when the app comes and goes. */
class DomovoiApp : Application() {
    val core: Core by lazy { Core(this) }
    private val surfaces by lazy { SystemSurfaces(this, core) }

    override fun onCreate() {
        super.onCreate()
        core.launch()
        surfaces.start()
        ProcessLifecycleOwner.get().lifecycle.addObserver(object : DefaultLifecycleObserver {
            override fun onStart(owner: LifecycleOwner) = core.enterForeground()
            override fun onResume(owner: LifecycleOwner) = core.setFocused(true)
            override fun onPause(owner: LifecycleOwner) = core.setFocused(false)
            override fun onStop(owner: LifecycleOwner) {
                core.enterBackground()
                surfaces.refreshShortcuts()
            }
        })
    }

    companion object {
        fun core(context: Context): Core = (context.applicationContext as DomovoiApp).core
    }
}
