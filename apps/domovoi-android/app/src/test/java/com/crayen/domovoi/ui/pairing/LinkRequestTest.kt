package com.crayen.domovoi.ui.pairing

import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.LinkRequestView
import com.crayen.domovoi.connectLinkEvent
import org.junit.Assert.assertEquals
import org.junit.Test

/** A link from outside the app: what is sent to the core, and what its question shows. */
class LinkRequestTest {
    private fun request(linkName: String = "", knownLabel: String = "") = LinkRequestView(
        id = 7u,
        title = "Connect to a new server?",
        message = "A link asks this app to connect.",
        confirmLabel = "Connect",
        endpoint = "a1b2c3d4e5",
        linkName = linkName,
        knownLabel = knownLabel,
    )

    @Test fun aLinkTheSystemHandsOverAsksFirst() {
        val link = "domovoi://connect?ticket=abc&code=123456"
        // Not `ConnectRemote`, which connects at once.
        assertEquals(Event.OpenLink(link), connectLinkEvent(link))
    }

    @Test fun theKeyIsAlwaysShownInAFixedWidthFace() {
        assertEquals(listOf(LinkRequestLine("Server key", "a1b2c3d4e5…", monospace = true)), linkRequestLines(request()))
    }

    @Test fun aSavedServerShowsTheNameItHasHere() {
        assertEquals(
            listOf(LinkRequestLine("Server key", "a1b2c3d4e5…", monospace = true), LinkRequestLine("Saved here as", "Studio")),
            linkRequestLines(request(knownLabel = "Studio")),
        )
    }

    @Test fun theLinksNameIsNeverShownForASavedServer() {
        val lines = linkRequestLines(request(linkName = "Your bank", knownLabel = "Studio"))
        assertEquals(listOf("Server key", "Saved here as"), lines.map { it.label })
        assertEquals("Studio", lines.last().value)
    }

    @Test fun aServerNotSavedShowsTheNameAsTheLinks() {
        assertEquals(
            listOf(LinkRequestLine("Server key", "a1b2c3d4e5…", monospace = true), LinkRequestLine("Name in the link", "Your bank")),
            linkRequestLines(request(linkName = "Your bank")),
        )
    }
}
