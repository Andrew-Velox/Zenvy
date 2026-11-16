package app.zenvy.remote

import java.net.URI

/**
 * A Zenvy PC: its address, the SHA-256 fingerprint of the certificate it must present, and an
 * optional one-time pairing secret. Parsed from the QR link the agent prints
 * (`https://192.168.0.230:7878/#s=<secret>&fp=<sha256 hex>`); `zenvy://` links have the same shape.
 *
 * Pure Kotlin on purpose (no android.* imports) so it is unit-testable and cannot be tricked by
 * platform URL parsing quirks.
 */
data class PairLink(val host: String, val port: Int, val fingerprint: String, val secret: String?) {

    /** `https://host:port` — the only origin the WebView is allowed to load. */
    val origin: String get() = "https://$host:$port"

    /** URL to open: with the one-time secret in the fragment while pairing, bare afterwards. */
    fun url(): String = if (secret != null) "$origin/#s=$secret&fp=$fingerprint" else "$origin/"

    fun withoutSecret() = copy(secret = null)

    /** True only for https to exactly this host and port. */
    fun isSameOrigin(url: String): Boolean {
        val u = try { URI(url) } catch (_: Exception) { return false }
        val p = if (u.port == -1) 443 else u.port
        return u.scheme.equals("https", ignoreCase = true) && u.host != null && ("[" + u.host.trim('[', ']') + "]").let { bracketed ->
            u.host.equals(host, ignoreCase = true) || bracketed.equals(host, ignoreCase = true)
        } && p == port
    }

    companion object {
        const val DEFAULT_PORT = 7878
        private val HOST = Regex("""^([A-Za-z0-9]([A-Za-z0-9.\-]{0,251}[A-Za-z0-9])?|\[[0-9A-Fa-f:.]{2,45}\])$""")
        private val SECRET = Regex("^[A-Za-z0-9_-]{16,64}$")
        private val HEX64 = Regex("^[0-9a-f]{64}$")

        /** Returns null for anything that is not a well-formed Zenvy link. Never throws. */
        fun parse(text: String?): PairLink? {
            val raw = text?.trim() ?: return null
            if (raw.length > 512 || raw.any { it.isISOControl() || it == ' ' }) return null
            val uri = try { URI(raw) } catch (_: Exception) { return null }
            if (uri.scheme?.lowercase() !in setOf("https", "zenvy")) return null
            val host = uri.host ?: return null
            if (!HOST.matches(host)) return null
            val port = if (uri.port == -1) DEFAULT_PORT else uri.port
            if (port !in 1..65535) return null
            val frag = uri.rawFragment ?: return null
            val params = frag.split('&').mapNotNull { kv -> kv.split('=', limit = 2).takeIf { it.size == 2 }?.let { it[0] to it[1] } }.toMap()
            val fp = params["fp"]?.replace(":", "")?.lowercase() ?: return null
            if (!HEX64.matches(fp)) return null
            val secret = params["s"]
            if (secret != null && !SECRET.matches(secret)) return null
            return PairLink(host, port, fp, secret)
        }
    }
}
