package com.crayen.domovoi.ui.tools

import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.RowScope
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.ArrowBack
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.ui.text.style.TextOverflow

/** A tool or settings screen: an app bar with back and actions over `content`. */
@Composable
fun ToolScaffold(
    title: String,
    onBack: (() -> Unit)?,
    actions: @Composable RowScope.() -> Unit = {},
    floatingActionButton: @Composable () -> Unit = {},
    bottomBar: @Composable () -> Unit = {},
    content: @Composable (PaddingValues) -> Unit,
) {
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                navigationIcon = { onBack?.let { IconButton(onClick = it) { Icon(Icons.AutoMirrored.Outlined.ArrowBack, "Back") } } },
                actions = actions,
            )
        },
        floatingActionButton = floatingActionButton,
        bottomBar = bottomBar,
        content = content,
    )
}
