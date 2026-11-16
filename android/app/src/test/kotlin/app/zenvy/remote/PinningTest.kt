package app.zenvy.remote

import org.junit.Assert.*
import org.junit.Test

class PinningTest {
    private val abc = "abc".toByteArray()
    private val abcHash = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad" // NIST test vector

    @Test fun hashMatchesKnownVector() = assertEquals(abcHash, Pinning.sha256Hex(abc))
    @Test fun matchesPinnedCaseInsensitively() { assertTrue(Pinning.matches(abc, abcHash)); assertTrue(Pinning.matches(abc, abcHash.uppercase())) }
    @Test fun rejectsDifferentCertificate() = assertFalse(Pinning.matches("abd".toByteArray(), abcHash))
    @Test fun rejectsMissingCertificateOrPin() { assertFalse(Pinning.matches(null, abcHash)); assertFalse(Pinning.matches(abc, "")); assertFalse(Pinning.matches(abc, abcHash.dropLast(1))) }
}
