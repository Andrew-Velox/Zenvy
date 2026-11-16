package app.zenvy.remote

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.http.SslCertificate
import android.net.http.SslError
import android.os.Build
import android.os.Bundle
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.webkit.*
import android.widget.*
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning

/**
 * Zenvy for Android: a thin, locked-down shell around the web UI the PC agent serves.
 *
 *  - Trusts exactly one certificate: the SHA-256 fingerprint from the pairing QR (no click-through
 *    warning, no user-installed CA, and a different certificate is refused outright).
 *  - Loads only the paired PC's https origin; any other navigation is blocked.
 *  - The device key lives in the WebView's app-private storage and is non-extractable.
 */
class MainActivity : Activity() {
    private lateinit var store: PinStore
    private lateinit var web: WebView
    private lateinit var setupPanel: View
    private lateinit var errorPanel: View
    private lateinit var errorText: TextView
    private lateinit var linkInput: EditText
    private var pin: PairLink? = null
    /** Text another app shared to Zenvy. It is only ever handed to the page as a string, and the person confirms what to do with it. */
    private var pendingShare: String = ""

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = PinStore(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) // a remote control should not sleep mid-use
        WebView.setWebContentsDebuggingEnabled(BuildConfig.DEBUG)

        web = buildWebView()
        setupPanel = buildSetupPanel()
        errorPanel = buildErrorPanel()
        val root = FrameLayout(this).apply {
            setBackgroundColor(c(R.color.surface))
            addView(web, match())
            addView(setupPanel, match())
            addView(errorPanel, match())
        }
        setContentView(root)

        captureShare(intent)
        val fromIntent = intent?.dataString
        when {
            fromIntent != null && handleLink(fromIntent) -> {}
            store.load() != null -> { pin = store.load(); open(pin!!.url()) }
            else -> showSetup()
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        intent.dataString?.let { handleLink(it) }
        if (captureShare(intent)) web.evaluateJavascript("window.zenvyShareCheck&&zenvyShareCheck()", null) // fixed script: the text is pulled by the page, never pasted into JS
    }

    /** Remembers text shared with ACTION_SEND (plain text only, bounded). Returns true when there is something new. */
    private fun captureShare(i: Intent?): Boolean {
        if (i?.action != Intent.ACTION_SEND || i.type != "text/plain") return false
        val t = i.getStringExtra(Intent.EXTRA_TEXT)?.trim().orEmpty()
        if (t.isEmpty()) return false
        pendingShare = t.take(12_000)
        return true
    }

    // ------------------------------------------------------------------ navigation / state

    private fun showSetup() {
        web.visibility = View.GONE; errorPanel.visibility = View.GONE; setupPanel.visibility = View.VISIBLE
    }

    private fun showError(msg: String) {
        errorText.text = msg
        web.visibility = View.GONE; setupPanel.visibility = View.GONE; errorPanel.visibility = View.VISIBLE
    }

    private fun open(url: String) {
        setupPanel.visibility = View.GONE; errorPanel.visibility = View.GONE; web.visibility = View.VISIBLE
        web.loadUrl(url)
    }

    /** Pins the PC described by a scanned/pasted/opened link and starts pairing. */
    private fun handleLink(text: String?): Boolean {
        val link = PairLink.parse(text)
        if (link == null) {
            Toast.makeText(this, "That is not a Zenvy pairing link", Toast.LENGTH_LONG).show()
            return false
        }
        store.save(link.withoutSecret())
        pin = link.withoutSecret()
        // A fresh pairing replaces any earlier identity for this app.
        WebStorage.getInstance().deleteAllData()
        open(link.url())
        return true
    }

    /** Forget the PC: drop the pin and the device key (WebView storage), return to setup. */
    fun forgetPc() {
        store.clear(); pin = null
        WebStorage.getInstance().deleteAllData()
        CookieManager.getInstance().removeAllCookies(null)
        web.clearCache(true); web.loadUrl("about:blank")
        showSetup()
    }

    @Deprecated("Deprecated in Java")
    override fun onBackPressed() {
        if (web.visibility == View.VISIBLE && web.canGoBack()) web.goBack() else @Suppress("DEPRECATION") super.onBackPressed()
    }

    // ------------------------------------------------------------------ WebView

    private fun buildWebView(): WebView = WebView(this).apply {
        setBackgroundColor(c(R.color.surface))
        overScrollMode = View.OVER_SCROLL_NEVER
        settings.apply {
            javaScriptEnabled = true          // the UI is a JS app
            domStorageEnabled = true          // IndexedDB holds the non-extractable device key
            allowFileAccess = false
            allowContentAccess = false
            setGeolocationEnabled(false)
            setSupportZoom(false)
            mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
            userAgentString = "$userAgentString ZenvyApp/${BuildConfig.VERSION_NAME}"
        }
        addJavascriptInterface(Bridge(), "ZenvyApp")
        webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean =
                pin?.isSameOrigin(request.url.toString()) != true // block everything that is not the paired PC

            override fun onReceivedSslError(view: WebView, handler: SslErrorHandler, error: SslError) {
                val p = pin
                if (p != null && Pinning.matches(derOf(error.certificate), p.fingerprint)) {
                    handler.proceed() // exactly the certificate from the QR code
                } else {
                    handler.cancel()
                    showError("This is not the PC you paired with. Its certificate does not match the one from the QR code.\n\nIf you regenerated the PC's certificate, forget this PC and pair again.")
                }
            }

            override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
                if (request.isForMainFrame && errorPanel.visibility != View.VISIBLE) {
                    showError("Can't reach ${pin?.host ?: "the PC"}. Check that zenvyd is running and that you are on the same network.")
                }
            }
        }
    }

    /** DER bytes of the server certificate. API 29+ exposes it directly; older versions via saveState. */
    private fun derOf(cert: SslCertificate?): ByteArray? {
        if (cert == null) return null
        return try {
            if (Build.VERSION.SDK_INT >= 29) cert.x509Certificate?.encoded
            else SslCertificate.saveState(cert).getByteArray("x509-certificate")
        } catch (_: Exception) { null }
    }

    /** What the web page may ask of the app. Only reachable from the pinned origin. */
    inner class Bridge {
        @JavascriptInterface fun deviceName(): String = Build.MODEL.take(40)
        @JavascriptInterface fun version(): String = BuildConfig.VERSION_NAME
        @JavascriptInterface fun forget() { runOnUiThread { forgetPc() } }
        @JavascriptInterface fun isDark(): Boolean = isNight()
        /** The phone's clipboard text, for "send to PC". Read only when the person taps the button in the app (the app is in front). */
        @JavascriptInterface fun clipboardRead(): String {
            val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            val clip = cm.primaryClip ?: return ""
            if (clip.itemCount == 0) return ""
            return clip.getItemAt(0).coerceToText(this@MainActivity)?.toString()?.take(12_000) ?: ""
        }
        /** Puts text on the phone's clipboard ("copy from PC"). */
        @JavascriptInterface fun clipboardWrite(text: String) {
            val cm = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            cm.setPrimaryClip(ClipData.newPlainText("Zenvy", text.take(65_536)))
        }
        /** The text another app shared with Zenvy, once. */
        @JavascriptInterface fun takeShare(): String { val t = pendingShare; pendingShare = ""; return t }
        /** The page tells us its surface colours so the system bars and WebView background match the theme. */
        @JavascriptInterface fun setChrome(bg: String, nav: String, lightIcons: Boolean) {
            val hex = Regex("^#[0-9a-fA-F]{6}$")
            if (!hex.matches(bg) || !hex.matches(nav)) return
            runOnUiThread { applyChrome(Color.parseColor(bg), Color.parseColor(nav), lightIcons) }
        }
    }

    private fun isNight() = (resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) == Configuration.UI_MODE_NIGHT_YES
    private fun c(id: Int) = getColor(id)

    private fun applyChrome(bg: Int, nav: Int, lightIcons: Boolean) {
        window.statusBarColor = bg
        window.navigationBarColor = nav
        web.setBackgroundColor(bg)
        @Suppress("DEPRECATION")
        run {
            var flags = window.decorView.systemUiVisibility
            flags = if (lightIcons) flags or View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR else flags and View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR.inv()
            if (Build.VERSION.SDK_INT >= 26) {
                flags = if (lightIcons) flags or View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR else flags and View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR.inv()
            }
            window.decorView.systemUiVisibility = flags
        }
    }

    // ------------------------------------------------------------------ native screens (no XML)

    private fun match() = FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)

    private fun text(s: String, size: Float, color: Int = c(R.color.on_surface), bold: Boolean = false) = TextView(this).apply {
        text = s; textSize = size; setTextColor(color)
        setLineSpacing(0f, 1.2f)
        if (bold) typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
    }

    private fun shape(fill: Int, radiusDp: Int, stroke: Int? = null) = GradientDrawable().apply {
        setColor(fill); cornerRadius = dp(radiusDp).toFloat()
        if (stroke != null) setStroke(dp(1), stroke)
    }

    /** Material 3 buttons: filled (primary) and tonal (secondary container), fully rounded. */
    private fun button(label: String, filled: Boolean, onClick: () -> Unit) = Button(this).apply {
        text = label; isAllCaps = false; textSize = 15f; stateListAnimator = null
        typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
        setTextColor(c(if (filled) R.color.on_primary else R.color.on_secondary_container))
        background = shape(c(if (filled) R.color.primary else R.color.secondary_container), 28)
        minimumHeight = dp(56); minHeight = dp(56)
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, dp(56))
        setOnClickListener { onClick() }
    }

    private fun column(): LinearLayout = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL; gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(24), dp(24), dp(24), dp(96)); setBackgroundColor(c(R.color.surface))
        layoutParams = match()
    }

    private fun gap(h: Int) = View(this).apply { layoutParams = LinearLayout.LayoutParams(1, dp(h)) }

    private fun buildSetupPanel(): View = column().apply {
        addView(text("Pair with your PC", 28f))
        addView(gap(8))
        addView(text("Run zenvyd pair on the PC, then scan the QR code it shows.", 16f, c(R.color.on_surface_variant)))
        addView(gap(32))
        addView(button("Scan QR code", true) { scan() })
        addView(gap(24))
        addView(text("Or paste the pairing link", 14f, c(R.color.primary), bold = true))
        addView(gap(8))
        linkInput = EditText(this@MainActivity).apply {
            hint = "https://…"; setHintTextColor(c(R.color.on_surface_variant)); setTextColor(c(R.color.on_surface)); textSize = 16f
            background = shape(Color.TRANSPARENT, 16, c(R.color.outline)); setPadding(dp(16), dp(16), dp(16), dp(16))
            inputType = InputType.TYPE_TEXT_VARIATION_URI or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS; maxLines = 3
        }
        addView(linkInput)
        addView(gap(12))
        addView(button("Connect", false) { handleLink(linkInput.text.toString()) })
    }

    private fun buildErrorPanel(): View = column().apply {
        visibility = View.GONE
        addView(text("Not connected", 28f))
        addView(gap(8))
        errorText = text("", 16f, c(R.color.on_surface_variant))
        addView(errorText)
        addView(gap(32))
        addView(button("Try again", true) { pin?.let { open(it.url()) } ?: showSetup() })
        addView(gap(12))
        addView(button("Forget this PC", false) { forgetPc() })
    }

    private fun scan() {
        val opts = GmsBarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).enableAutoZoom().build()
        GmsBarcodeScanning.getClient(this, opts).startScan()
            .addOnSuccessListener { handleLink(it.rawValue) }
            .addOnFailureListener { Toast.makeText(this, "Scan failed or was cancelled. You can paste the link instead.", Toast.LENGTH_LONG).show() }
    }

}
