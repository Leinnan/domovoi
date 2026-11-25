package com.crayen.domovoi.system

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** The server's Web Push message (`push_web.rs`) as a notice to post. */
class UnifiedPushServiceTest {
    @Test
    fun anApprovalCarriesItsRequestAndAFinishedTurnNone() {
        val approval = UnifiedPushService.parse("""{"environmentId":"srv","threadId":"t1","title":"Fix the tests","body":"Needs your approval","requestId":"rq1"}""")!!
        assertEquals("t1", approval.threadId)
        assertEquals("srv", approval.environmentId)
        assertEquals("Fix the tests", approval.title)
        assertEquals("Needs your approval", approval.body)
        assertEquals("rq1", approval.requestId)
        assertNull(UnifiedPushService.parse("""{"environmentId":"srv","threadId":"t1","title":"x","body":"Finished"}""")!!.requestId)
    }

    @Test
    fun aMessageWithoutAThreadOrNotJsonIsDropped() {
        assertNull(UnifiedPushService.parse("""{"title":"x"}"""))
        assertNull(UnifiedPushService.parse("not json"))
    }
}
