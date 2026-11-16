package app.zenvy.remote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class DownloaderTest {
    @Test fun onlyDownloadTicketPathsAreAccepted() {
        assertTrue(Downloader.isValidPath("/download/" + "a1".repeat(24)))
        for (bad in listOf("", "/download/", "/download/xyz", "/download/" + "A1".repeat(24), "/upload/" + "a1".repeat(24),
            "https://evil.example/download/" + "a1".repeat(24), "/download/" + "a1".repeat(24) + "/x", "/download/" + "a1".repeat(24) + "?x=1", "//evil.example/x", "/download/../etc/passwd")) {
            assertFalse(bad, Downloader.isValidPath(bad))
        }
    }

    @Test fun fileNamesAreReducedToAPlainName() {
        assertEquals("passwd", Downloader.sanitizeName("../../etc/passwd"))
        assertEquals("evil.dll", Downloader.sanitizeName("C:\\Windows\\evil.dll"))
        assertEquals("bashrc", Downloader.sanitizeName(".bashrc"))
        assertEquals("report_ Q3.pdf", Downloader.sanitizeName("report: Q3.pdf"))
        assertEquals("zenvy-file", Downloader.sanitizeName("..."))
        assertEquals("zenvy-file", Downloader.sanitizeName(""))
        assertEquals("tabnewline.txt", Downloader.sanitizeName("tab\tnew\nline.txt"))
        assertTrue(Downloader.sanitizeName("x".repeat(500)).length <= 120)
    }

    @Test fun openRefusesAnythingButADownloadPath() {
        val pin = PairLink("192.168.0.230", 7878, "a".repeat(64), null)
        try { Downloader.open(pin, "/ws"); throw AssertionError("must refuse") } catch (_: IllegalArgumentException) {}
    }
}
