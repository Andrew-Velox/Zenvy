package app.zenvy.remote

import org.junit.Assert.*
import org.junit.Test

class PairLinkTest {
    private val fp = "ed260754c30976d63a6382286cd532e0119f6bb1d3fb41df4faea680bc6a0c18"
    private val secret = "E8Ia5BdikEabcdefghijkl"

    @Test fun parsesTheLinkTheAgentPrints() {
        val l = PairLink.parse("https://192.168.1.20:7878/#s=$secret&fp=${fp.uppercase()}")!!
        assertEquals("192.168.1.20", l.host); assertEquals(7878, l.port); assertEquals(fp, l.fingerprint); assertEquals(secret, l.secret)
        assertEquals("https://192.168.1.20:7878/#s=$secret&fp=$fp", l.url())
        assertEquals("https://192.168.1.20:7878/", l.withoutSecret().url())
    }

    @Test fun acceptsColonSeparatedFingerprintAndZenvyScheme() {
        val colon = fp.chunked(2).joinToString(":")
        assertEquals(fp, PairLink.parse("zenvy://pc.local:9000/#fp=$colon")!!.fingerprint)
        assertNull(PairLink.parse("zenvy://pc.local:9000/#fp=$colon")!!.secret)
        assertEquals(7878, PairLink.parse("zenvy://pc.local/#fp=$fp")!!.port)
    }

    @Test fun ipv6Hosts() {
        val l = PairLink.parse("https://[fd00::1]:7878/#fp=$fp")!!
        assertEquals("https://[fd00::1]:7878", l.origin)
        assertTrue(l.isSameOrigin("https://[fd00::1]:7878/ws"))
    }

    @Test fun rejectsAnythingMalformedOrDangerous() {
        val bad = listOf(
            null, "", "   ", "garbage",
            "http://192.168.1.20:7878/#fp=$fp",                 // plain http is never accepted
            "javascript:alert(1)", "file:///etc/passwd", "intent://x#Intent;end",
            "https://192.168.1.20:7878/",                       // no fingerprint
            "https://192.168.1.20:7878/#fp=abc",                // short fingerprint
            "https://192.168.1.20:7878/#fp=${"z".repeat(64)}",  // not hex
            "https://192.168.1.20:70000/#fp=$fp",               // bad port
            "https://192.168.1.20:0/#fp=$fp",
            "https://exa mple.com:7878/#fp=$fp",                 // whitespace in host
            "https://192.168.1.20:7878/#s=short&fp=$fp",        // secret too short
            "https://192.168.1.20:7878/#s=${"a".repeat(65)}&fp=$fp",
            "https://192.168.1.20:7878/#s=a b c d e f g h i j k l&fp=$fp",
            "https://user:pw@192.168.1.20:7878/#fp=$fp".replace("user:pw@", "evil.example\u0000@"),
            "https://" + "a".repeat(300) + ":7878/#fp=$fp",
        )
        for (b in bad) assertNull("should reject: $b", PairLink.parse(b))
    }

    @Test fun userInfoTrickDoesNotChangeTheHost() {
        // "https://trusted@evil" must resolve to host=evil, never to "trusted".
        val l = PairLink.parse("https://192.168.1.20@evil.example:7878/#fp=$fp")
        assertTrue(l == null || l.host == "evil.example")
    }

    @Test fun sameOriginIsStrict() {
        val l = PairLink.parse("https://192.168.1.20:7878/#fp=$fp")!!
        assertTrue(l.isSameOrigin("https://192.168.1.20:7878/"))
        assertTrue(l.isSameOrigin("https://192.168.1.20:7878/ws?x=1"))
        assertFalse(l.isSameOrigin("http://192.168.1.20:7878/"))
        assertFalse(l.isSameOrigin("https://192.168.1.20:7879/"))
        assertFalse(l.isSameOrigin("https://192.168.1.21:7878/"))
        assertFalse(l.isSameOrigin("https://192.168.1.20.evil.example:7878/"))
        assertFalse(l.isSameOrigin("https://evil.example/?https://192.168.1.20:7878/"))
        assertFalse(l.isSameOrigin("javascript:void(0)"))
        assertFalse(l.isSameOrigin("not a url"))
    }

    @Test fun `the host for a socket has no brackets around an IPv6 address`() {
        val fp = "a".repeat(64)
        assertEquals("fd00::1", PairLink.parse("https://[fd00::1]:7878/#fp=$fp")!!.connectHost)
        assertEquals("192.168.1.20", PairLink.parse("https://192.168.1.20:7878/#fp=$fp")!!.connectHost)
        assertEquals("my-pc.local", PairLink.parse("https://my-pc.local:7878/#fp=$fp")!!.connectHost)
    }
}
