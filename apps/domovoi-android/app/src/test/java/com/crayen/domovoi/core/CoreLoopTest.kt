package com.crayen.domovoi.core

import com.crayen.domovoi.app.Effect
import com.crayen.domovoi.app.Event
import com.crayen.domovoi.app.RenderOperation
import com.crayen.domovoi.app.Request
import com.crayen.domovoi.app.Requests
import com.crayen.domovoi.app.StoreOperation
import com.crayen.domovoi.app.ViewDelta
import kotlinx.coroutines.launch
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/** The loop against a stand-in for the Rust core. */
class CoreLoopTest {
    /** Asks for a store read named after each tick it gets, and a render. */
    private class Bridge : CoreBridge {
        val threads: MutableSet<Thread> = Collections.synchronizedSet(mutableSetOf())
        val ticks: MutableList<Long> = Collections.synchronizedList(mutableListOf())
        val views = AtomicInteger()
        /** How many ticks the core had taken when the view was last read. */
        @Volatile var ticksAtLastView = 0

        private fun called() = threads.add(Thread.currentThread())

        override fun update(event: ByteArray): ByteArray {
            called()
            val tick = (Event.bincodeDeserialize(event) as Event.Tick).nowMs
            ticks.add(tick)
            return Requests(listOf(
                Request(1u, Effect.Store(StoreOperation.Load("k$tick"))),
                Request(2u, Effect.Render(RenderOperation)),
            )).bincodeSerialize()
        }

        override fun resolve(id: UInt, output: ByteArray): ByteArray {
            called()
            return Requests(listOf(Request(2u, Effect.Render(RenderOperation)))).bincodeSerialize()
        }

        override fun view(): ByteArray {
            called()
            views.incrementAndGet()
            ticksAtLastView = ticks.size
            return ViewDelta().bincodeSerialize()
        }
    }

    private val bridge = Bridge()
    private val handled: MutableList<String> = Collections.synchronizedList(mutableListOf())
    private val handlerThreads: MutableSet<Thread> = Collections.synchronizedSet(mutableSetOf())
    private val shown = AtomicInteger()
    private val loop = CoreLoop(
        bridge,
        handle = { request ->
            handlerThreads.add(Thread.currentThread())
            handled.add(((request.effect as Effect.Store).value as StoreOperation.Load).key)
        },
        show = { shown.incrementAndGet() },
    )

    @After fun stop() = loop.close()

    /** Waits until the loop has taken everything sent and shown the last render. */
    private fun settle(ticks: Int) {
        val deadline = System.currentTimeMillis() + 5_000
        while (System.currentTimeMillis() < deadline) {
            if (bridge.ticks.size == ticks && bridge.ticksAtLastView == ticks && shown.get() == bridge.views.get()) return
            Thread.sleep(5)
        }
        fail("the loop did not settle: ${bridge.ticks.size} of $ticks ticks, view read at ${bridge.ticksAtLastView}")
    }

    @Test fun everyCallIntoTheCoreIsOnItsOwnThread() {
        val start = CountDownLatch(1)
        val senders = (0 until 4).map { sender ->
            Thread {
                start.await()
                repeat(50) { loop.send(Event.Tick((sender * 1000 + it).toLong())) }
            }.also { it.start() }
        }
        start.countDown()
        senders.forEach { it.join() }
        settle(200)
        assertOneCoreThread(bridge.threads)
        assertEquals("effects are handled where the core runs", bridge.threads.toSet(), handlerThreads.toSet())
        // Each sender's events are taken in the order it sent them.
        for (sender in 0 until 4) {
            val mine = bridge.ticks.filter { it / 1000 == sender.toLong() }
            assertEquals(mine.sorted(), mine)
        }
    }

    @Test fun eventsAndTheirEffectsKeepTheOrderSent() {
        repeat(300) { loop.send(Event.Tick(it.toLong())) }
        settle(300)
        assertEquals((0L until 300L).toList(), bridge.ticks.toList())
        assertEquals((0 until 300).map { "k$it" }, handled.toList())
    }

    @Test fun aBurstOfRendersIsReadOnceAndTheLastOneIsShown() {
        repeat(500) { loop.send(Event.Tick(it.toLong())) }
        settle(500)
        // 500 renders asked for within a few milliseconds: a handful of reads, not 500.
        assertTrue("views read: ${bridge.views.get()}", bridge.views.get() in 1..25)
        assertEquals("the view was read after the last event", 500, bridge.ticksAtLastView)
        assertEquals(bridge.views.get(), shown.get())
    }

    @Test fun anAnswerIsGivenOnTheCoresThreadOnly() {
        try {
            loop.resolve(1u, ByteArray(0))
            fail("an answer from another thread went into the core")
        } catch (expected: IllegalStateException) {
            // The core's calls stay on its thread.
        }
        val done = CountDownLatch(1)
        loop.scope.launch {
            loop.resolve(1u, ByteArray(0))
            done.countDown()
        }
        assertTrue(done.await(5, TimeUnit.SECONDS))
        assertOneCoreThread(bridge.threads)
    }

    @Test fun aReplacedCoreIsResetOnceAndItsWholeViewRead() {
        /** Reports, after its first event, that it was replaced after a panic. */
        class Replaced : CoreBridge {
            val events = AtomicInteger()
            val views = AtomicInteger()
            @Volatile var replaced = false
            override fun update(event: ByteArray): ByteArray {
                if (events.incrementAndGet() == 1) replaced = true
                return Requests(emptyList()).bincodeSerialize()
            }
            override fun resolve(id: UInt, output: ByteArray) = Requests(emptyList()).bincodeSerialize()
            override fun view(): ByteArray {
                views.incrementAndGet()
                return ViewDelta().bincodeSerialize()
            }
            override fun takeReset() = replaced.also { replaced = false }
        }
        val core = Replaced()
        val resets = AtomicInteger()
        val viewsAtReset = AtomicInteger(-1)
        val shownAfter = CountDownLatch(1)
        val replacedLoop = CoreLoop(
            core,
            handle = {},
            show = { if (resets.get() > 0) shownAfter.countDown() },
            reset = {
                viewsAtReset.set(core.views.get())
                resets.incrementAndGet()
            },
        )
        try {
            replacedLoop.send(Event.Tick(1))
            replacedLoop.send(Event.Tick(2))
            assertTrue("the new core's view is read and shown", shownAfter.await(5, TimeUnit.SECONDS))
            Thread.sleep(50)
            assertEquals("told once", 1, resets.get())
            assertEquals("before the view that follows is read", 0, viewsAtReset.get())
        } finally {
            replacedLoop.close()
        }
    }

    private fun assertOneCoreThread(threads: Set<Thread>) {
        assertEquals("one thread calls into the core", 1, threads.size)
        // The test runtime appends the coroutine's name.
        assertTrue(threads.first().name.startsWith("domovoi-core"))
        assertTrue(threads.first() !== Thread.currentThread())
    }
}
