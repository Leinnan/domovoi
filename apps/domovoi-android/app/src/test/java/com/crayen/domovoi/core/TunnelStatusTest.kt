package com.crayen.domovoi.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class TunnelStatusTest {
    @Test fun readsTheBridgeStatus() {
        val s = Tunnel.parseStatus("""{"connected":true,"path":"direct","rttMs":12,"error":null,"listening":true}""")!!
        assertEquals(true, s.connected)
        assertEquals("direct", s.path)
        assertEquals(12u, s.rttMs)
        assertEquals("", s.error)
        assertEquals(true, s.listening)
    }

    @Test fun clampsAndDefaults() {
        val s = Tunnel.parseStatus("""{"connected":false,"path":null,"rttMs":999999}""")!!
        assertEquals("", s.path)
        assertEquals(60_000u, s.rttMs)
        assertEquals(true, s.listening)
    }

    @Test fun stoppedIsNull() {
        assertNull(Tunnel.parseStatus("null"))
    }
}
