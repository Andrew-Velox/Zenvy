package app.zenvy.remote

import java.net.URL
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.HostnameVerifier
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.X509TrustManager

/**
 * Downloads one file the PC offered. The connection trusts exactly one certificate (the pinned one from the pairing QR),
 * only `/download/<ticket>` paths are accepted, and the file name is reduced to a plain name. No android.* imports on
 * purpose, so it is unit-testable.
 */
object Downloader {
    private val PATH = Regex("^/download/[0-9a-f]{48}$")

    fun isValidPath(path: String) = PATH.matches(path)

    /** The last path component only, no control characters, no leading dots, characters that trouble other systems replaced. */
    fun sanitizeName(raw: String): String {
        val last = raw.substringAfterLast('/').substringAfterLast('\\')
        val s = last.filter { !it.isISOControl() }.replace(Regex("[<>:\"|?*]"), "_").trim().trimStart('.').trim().trimEnd('.', ' ')
        return if (s.isEmpty() || s.all { it == '_' }) "zenvy-file" else s.take(120)
    }

    fun open(pin: PairLink, path: String): HttpsURLConnection {
        require(isValidPath(path)) { "not a download path" }
        val trust = object : X509TrustManager {
            override fun checkClientTrusted(chain: Array<X509Certificate>?, authType: String?) {}
            override fun checkServerTrusted(chain: Array<X509Certificate>?, authType: String?) {
                if (chain.isNullOrEmpty() || !Pinning.matches(chain[0].encoded, pin.fingerprint)) throw CertificateException("this is not the PC you paired with")
            }
            override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
        }
        val ctx = SSLContext.getInstance("TLS").apply { init(null, arrayOf(trust), null) }
        val bare = pin.host.trim('[', ']')
        return (URL(pin.origin + path).openConnection() as HttpsURLConnection).apply {
            sslSocketFactory = ctx.socketFactory
            hostnameVerifier = HostnameVerifier { h, _ -> h.equals(bare, ignoreCase = true) }
            connectTimeout = 10_000
            readTimeout = 30_000
            instanceFollowRedirects = false
        }
    }
}
