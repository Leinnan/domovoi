package com.crayen.domovoi.core

import com.crayen.domovoi.app.LocalOperation
import com.crayen.domovoi.app.LocalOutput
import org.junit.Assert.assertEquals
import org.junit.Test

/** The core's search for a server on the same computer, answered on a phone. */
class LocalAnswerTest {
    @Test fun thereIsNoDescriptorToWatch() {
        assertEquals(LocalOutput.Descriptor(null), localAnswer(LocalOperation.WATCH))
    }

    @Test fun thereIsNowhereToWriteARequest() {
        assertEquals(LocalOutput.Unavailable, localAnswer(LocalOperation.REQUEST))
    }

    @Test fun everyOperationHasAnAnswerTheCoreCanRead() {
        for (operation in LocalOperation.entries) {
            val answer = localAnswer(operation)
            assertEquals(answer, LocalOutput.bincodeDeserialize(answer.bincodeSerialize()))
        }
    }
}
