package app.zenvy.remote

import android.content.Context

/** The one paired PC, in app-private storage (excluded from backups, see data_extraction_rules.xml). */
class PinStore(context: Context) {
    private val prefs = context.getSharedPreferences("zenvy_pin", Context.MODE_PRIVATE)

    fun load(): PairLink? {
        val host = prefs.getString("host", null) ?: return null
        val fp = prefs.getString("fp", null) ?: return null
        val port = prefs.getInt("port", PairLink.DEFAULT_PORT)
        // Re-validate through the same parser so a corrupted value can never be trusted.
        return PairLink.parse("https://${host}:$port/#fp=$fp")
    }

    fun save(link: PairLink) {
        prefs.edit().putString("host", link.host).putInt("port", link.port).putString("fp", link.fingerprint).apply()
    }

    fun clear() = prefs.edit().clear().apply()
}
