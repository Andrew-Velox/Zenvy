package app.zenvy.remote

import java.security.MessageDigest

/** Certificate pinning: the PC is trusted only if its certificate hashes to the pinned fingerprint. */
object Pinning {
    fun sha256Hex(der: ByteArray): String = MessageDigest.getInstance("SHA-256").digest(der).joinToString("") { "%02x".format(it) }

    /** Constant-time comparison of the certificate's SHA-256 with the pinned hex fingerprint. */
    fun matches(der: ByteArray?, pinnedHex: String): Boolean {
        if (der == null) return false
        return MessageDigest.isEqual(sha256Hex(der).toByteArray(), pinnedHex.lowercase().toByteArray())
    }
}
