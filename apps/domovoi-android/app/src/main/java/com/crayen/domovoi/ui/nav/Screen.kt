package com.crayen.domovoi.ui.nav

/** Where the app can be (`RootView.Screen`). */
sealed interface Screen {
    data object Threads : Screen
    data class Thread(val id: String) : Screen
    data object NewThread : Screen
    data object Diff : Screen
    data object Terminal : Screen
    data object Files : Screen
    data object Git : Screen
    data object ImportSessions : Screen
    data object BrowseFolders : Screen
    data object AddServer : Screen
    data object PullRequests : Screen
    data object Usage : Screen
    data object Settings : Screen
    data object ServerSettings : Screen
    data object Providers : Screen
    data object ScheduledTasks : Screen
    data object Maintenance : Screen
    data object Access : Screen
}

/** What screens use to move around. */
interface Navigator {
    fun open(screen: Screen)
    fun back()
    /** Replaces what the detail pane shows (a thread opened from the list). */
    fun show(screen: Screen)
}
