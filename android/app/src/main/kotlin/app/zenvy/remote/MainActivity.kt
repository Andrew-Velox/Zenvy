package app.zenvy.remote

import android.app.Activity
import android.content.ClipData
import android.content.ContentValues
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.res.ColorStateList
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.net.http.SslCertificate
import android.net.http.SslError
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.os.Environment
import android.provider.MediaStore
import java.io.File
import java.io.IOException
import java.io.InputStream
import java.net.InetSocketAddress
import java.net.Socket
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
    private companion object {
        const val REQ_FILES = 41
        const val PROBE_TIMEOUT_MS = 2500
        const val LOAD_TIMEOUT_MS = 20_000L
        const val RETRY_MS = 4_000L
    }
    private enum class Screen { SETUP, CONNECTING, ERROR, WEB }
    private enum class Fail { UNREACHABLE, SLOW, CERT }
    private val watchdog = Runnable { loadFailed = true; web.stopLoading(); fail(Fail.SLOW) }
    private val autoRetry = Runnable { if (screen == Screen.ERROR && failKind != Fail.CERT && resumed) connect(silent = true) }
    private lateinit var store: PinStore
    private lateinit var web: WebView
    private lateinit var setupPanel: View
    private lateinit var connectingPanel: View
    private lateinit var connectingHost: TextView
    private lateinit var errorPanel: View
    private lateinit var errIcon: ImageView
    private lateinit var errTitle: TextView
    private lateinit var errHost: TextView
    private lateinit var errBody: TextView
    private lateinit var errChecklist: View
    private lateinit var errPort: TextView
    private lateinit var retryBtn: Button
    private lateinit var forgetBtn: Button
    private lateinit var pasteGroup: View
    private lateinit var linkInput: EditText
    private val ui = Handler(Looper.getMainLooper())
    private var screen = Screen.SETUP
    private var failKind = Fail.UNREACHABLE
    /** Bumped on every attempt so an answer from an older attempt is ignored. */
    private var attempt = 0
    private var loadFailed = false
    private var resumed = false
    /** The link with the one-time pairing secret, kept until the first page has loaded so an automatic retry still pairs. */
    private var pendingUrl: String? = null
    private var pin: PairLink? = null
    /** Text another app shared to Zenvy. It is only ever handed to the page as a string, and the person confirms what to do with it. */
    private var pendingShare: String = ""
    /** The pending "choose files" request from the page (<input type=file>). */
    private var fileChooser: ValueCallback<Array<Uri>>? = null

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        store = PinStore(this)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) // a remote control should not sleep mid-use
        WebView.setWebContentsDebuggingEnabled(BuildConfig.DEBUG)

        web = buildWebView()
        setupPanel = buildSetupPanel()
        connectingPanel = buildConnectingPanel()
        errorPanel = buildErrorPanel()
        val root = FrameLayout(this).apply {
            setBackgroundColor(c(R.color.surface))
            addView(web, match())
            addView(setupPanel, match())
            addView(connectingPanel, match())
            addView(errorPanel, match())
        }
        show(Screen.SETUP)
        setContentView(root)

        captureShare(intent)
        val fromIntent = intent?.dataString
        when {
            fromIntent != null && handleLink(fromIntent) -> {}
            store.load() != null -> { pin = store.load(); connect() }
            else -> show(Screen.SETUP)
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

    private fun show(next: Screen) {
        screen = next
        setupPanel.visibility = if (next == Screen.SETUP) View.VISIBLE else View.GONE
        connectingPanel.visibility = if (next == Screen.CONNECTING) View.VISIBLE else View.GONE
        errorPanel.visibility = if (next == Screen.ERROR) View.VISIBLE else View.GONE
        // the page stays out of sight (but keeps loading) until it has really loaded: never a stale browser error page
        web.visibility = if (next == Screen.WEB) View.VISIBLE else View.INVISIBLE
    }

    /**
     * Opens the paired PC. A quick TCP probe tells within a couple of seconds whether anything answers; only then is the
     * page loaded, behind a "Connecting" screen, and shown once it has finished. `silent` is the automatic retry on the
     * "can't reach" screen: it changes nothing on screen until the PC answers.
     */
    private fun connect(silent: Boolean = false) {
        val p = pin ?: run { show(Screen.SETUP); return }
        val mine = ++attempt
        val started = SystemClock.uptimeMillis()
        ui.removeCallbacks(autoRetry); ui.removeCallbacks(watchdog)
        loadFailed = false
        if (!silent) {
            if (screen == Screen.ERROR) setRetrying(true)
            else { connectingHost.text = "${p.host}:${p.port}"; show(Screen.CONNECTING) }
        }
        Thread {
            val up = reachable(p)
            ui.post {
                if (mine != attempt) return@post
                when {
                    !up && silent -> ui.postDelayed(autoRetry, RETRY_MS)
                    // a refusal comes back in milliseconds; keep "Connecting…" up for a moment so the tap is seen to do something
                    !up -> ui.postDelayed({ if (mine == attempt) fail(Fail.UNREACHABLE) }, (900 - (SystemClock.uptimeMillis() - started)).coerceAtLeast(0))
                    else -> {
                        connectingHost.text = "${p.host}:${p.port}"; show(Screen.CONNECTING)
                        web.loadUrl(pendingUrl ?: p.url())
                        ui.postDelayed(watchdog, LOAD_TIMEOUT_MS)
                    }
                }
            }
        }.start()
    }

    private fun reachable(p: PairLink): Boolean = try {
        Socket().use { it.connect(InetSocketAddress(p.connectHost, p.port), PROBE_TIMEOUT_MS); true }
    } catch (_: Exception) { false }

    private fun fail(kind: Fail) {
        ui.removeCallbacks(watchdog); ui.removeCallbacks(autoRetry)
        failKind = kind; setRetrying(false)
        val p = pin
        val cert = kind == Fail.CERT
        errIcon.setImageResource(if (cert) R.drawable.ic_shield_off else R.drawable.ic_wifi_off)
        errIcon.imageTintList = ColorStateList.valueOf(c(if (cert) R.color.error else R.color.primary))
        errTitle.text = when (kind) { Fail.UNREACHABLE -> "Can't reach your PC"; Fail.SLOW -> "Your PC isn't answering"; Fail.CERT -> "This isn't your PC" }
        errHost.text = if (p == null) "" else "${p.host}:${p.port}"
        errHost.visibility = if (p == null) View.GONE else View.VISIBLE
        errBody.text = if (cert) "Its certificate does not match the one from the QR code. If you regenerated the certificate on the PC, forget it here and pair again."
            else "Zenvy keeps trying and reconnects by itself as soon as the PC answers."
        errChecklist.visibility = if (cert) View.GONE else View.VISIBLE
        errPort.text = "Port ${p?.port ?: PairLink.DEFAULT_PORT} is open in the firewall"
        restyle(retryBtn, !cert); restyle(forgetBtn, cert)
        show(Screen.ERROR)
        if (!cert && resumed) ui.postDelayed(autoRetry, RETRY_MS)
    }

    private fun setRetrying(on: Boolean) {
        retryBtn.isEnabled = !on
        retryBtn.text = if (on) "Connecting…" else "Try again"
        retryBtn.alpha = if (on) 0.6f else 1f
    }

    override fun onResume() {
        super.onResume()
        resumed = true
        if (screen == Screen.ERROR && failKind != Fail.CERT) { ui.removeCallbacks(autoRetry); ui.postDelayed(autoRetry, 1_000) }
    }

    override fun onPause() {
        resumed = false
        ui.removeCallbacks(autoRetry)
        super.onPause()
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
        pendingUrl = link.url()
        // A fresh pairing replaces any earlier identity for this app.
        WebStorage.getInstance().deleteAllData()
        connect()
        return true
    }

    /** Forget the PC: drop the pin and the device key (WebView storage), return to setup. */
    fun forgetPc() {
        store.clear(); pin = null; pendingUrl = null
        attempt++; ui.removeCallbacks(autoRetry); ui.removeCallbacks(watchdog)
        WebStorage.getInstance().deleteAllData()
        CookieManager.getInstance().removeAllCookies(null)
        web.clearCache(true); web.loadUrl("about:blank")
        show(Screen.SETUP)
    }

    @Deprecated("Deprecated in Java")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        if (requestCode == REQ_FILES) {
            fileChooser?.onReceiveValue(WebChromeClient.FileChooserParams.parseResult(resultCode, data))
            fileChooser = null
        } else @Suppress("DEPRECATION") super.onActivityResult(requestCode, resultCode, data)
    }

    /** Downloads a file the PC offered into Downloads/Zenvy, over a connection that trusts only the pinned certificate. */
    private fun downloadFromPc(path: String, rawName: String) {
        val p = pin
        if (p == null || !Downloader.isValidPath(path)) { toast("That download is not valid"); return }
        val name = Downloader.sanitizeName(rawName)
        Thread {
            try {
                val c = Downloader.open(p, path)
                if (c.responseCode != 200) throw IOException("the PC answered ${c.responseCode}")
                c.inputStream.use { saveDownload(name, it) }
                toast("Saved $name to Downloads/Zenvy")
            } catch (e: Exception) {
                toast("Download failed: ${e.message?.take(80) ?: "error"}")
            }
        }.start()
    }

    private fun saveDownload(name: String, input: InputStream) {
        if (Build.VERSION.SDK_INT >= 29) {
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream")
                put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS + "/Zenvy")
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
            val uri = contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values) ?: throw IOException("cannot create the file")
            try {
                (contentResolver.openOutputStream(uri) ?: throw IOException("cannot write the file")).use { input.copyTo(it, 64 * 1024) }
                values.clear(); values.put(MediaStore.Downloads.IS_PENDING, 0)
                contentResolver.update(uri, values, null, null)
            } catch (e: Exception) { contentResolver.delete(uri, null, null); throw e }
        } else {
            val dir = File(getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS), "Zenvy").apply { mkdirs() }
            File(dir, name).outputStream().use { input.copyTo(it, 64 * 1024) }
        }
    }

    private fun toast(msg: String) = runOnUiThread { Toast.makeText(this, msg, Toast.LENGTH_LONG).show() }

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
        webChromeClient = object : WebChromeClient() {
            // Lets the page pick files to send to the PC. The page only ever receives the chosen files, nothing else on the phone.
            override fun onShowFileChooser(w: WebView, cb: ValueCallback<Array<Uri>>, params: FileChooserParams): Boolean {
                fileChooser?.onReceiveValue(null)
                fileChooser = cb
                return try {
                    startActivityForResult(params.createIntent(), REQ_FILES); true
                } catch (_: Exception) { fileChooser = null; false }
            }
        }
        webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest): Boolean =
                pin?.isSameOrigin(request.url.toString()) != true // block everything that is not the paired PC

            override fun onReceivedSslError(view: WebView, handler: SslErrorHandler, error: SslError) {
                val p = pin
                if (p != null && Pinning.matches(derOf(error.certificate), p.fingerprint)) {
                    handler.proceed() // exactly the certificate from the QR code
                } else {
                    handler.cancel()
                    loadFailed = true
                    fail(Fail.CERT)
                }
            }

            override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) {
                if (!request.isForMainFrame || loadFailed) return
                loadFailed = true
                fail(Fail.UNREACHABLE)
            }

            override fun onPageFinished(view: WebView, url: String?) {
                // the paired PC's page loaded: now (and only now) it is shown
                if (!loadFailed && screen == Screen.CONNECTING && pin?.isSameOrigin(url.orEmpty()) == true) {
                    ui.removeCallbacks(watchdog)
                    pendingUrl = null
                    show(Screen.WEB)
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
        /** Download a file the PC offered (`/download/<ticket>` on the paired PC), saved to Downloads/Zenvy. */
        @JavascriptInterface fun download(path: String, name: String) { runOnUiThread { downloadFromPc(path, name) } }
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
        setPadding(dp(28), dp(32), dp(28), dp(56)); setBackgroundColor(c(R.color.surface))
        layoutParams = match()
    }

    private fun gap(h: Int) = View(this).apply { layoutParams = LinearLayout.LayoutParams(1, dp(h)) }

    /** Scrolls on small screens or in landscape; otherwise the content is centred. */
    private fun scroll(content: View): ScrollView = ScrollView(this).apply {
        isFillViewport = true; isVerticalScrollBarEnabled = false; setBackgroundColor(c(R.color.surface))
        addView(content, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
    }

    private fun centered(t: TextView) = t.apply { gravity = Gravity.CENTER }

    /** A small monospace pill for an address or a command. */
    private fun chip(s: String) = TextView(this).apply {
        text = s; textSize = 14f; typeface = Typeface.MONOSPACE; setTextColor(c(R.color.on_surface))
        background = shape(c(R.color.surface_container_high), 12); setPadding(dp(12), dp(6), dp(12), dp(6))
        layoutParams = LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT)
    }

    private fun restyle(b: Button, filled: Boolean) {
        b.setTextColor(c(if (filled) R.color.on_primary else R.color.on_secondary_container))
        b.background = shape(c(if (filled) R.color.primary else R.color.secondary_container), 28)
    }

    private fun dot() = View(this).apply {
        background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(c(R.color.primary)) }
        layoutParams = LinearLayout.LayoutParams(dp(7), dp(7)).apply { marginEnd = dp(12) }
    }

    private fun row(vararg views: View) = LinearLayout(this).apply {
        orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL
        views.forEach { addView(it) }
    }

    private fun stepRow(n: String, body: View) = row(
        TextView(this).apply {
            text = n; textSize = 14f; gravity = Gravity.CENTER; setTextColor(c(R.color.primary))
            typeface = Typeface.create("sans-serif-medium", Typeface.NORMAL)
            background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(c(R.color.secondary_container)) }
            layoutParams = LinearLayout.LayoutParams(dp(32), dp(32)).apply { marginEnd = dp(16) }
        }, body)

    private fun buildSetupPanel(): View = scroll(column().apply {
        gravity = Gravity.CENTER_VERTICAL
        addView(ImageView(this@MainActivity).apply {
            setImageResource(R.drawable.ic_launcher_monochrome); imageTintList = ColorStateList.valueOf(c(R.color.on_surface))
            layoutParams = LinearLayout.LayoutParams(dp(112), dp(112)).apply { marginStart = -dp(30); topMargin = -dp(20); bottomMargin = -dp(12) }
        })
        addView(text("Pair with your PC", 30f, bold = true))
        addView(gap(8))
        addView(text("It takes about ten seconds.", 16f, c(R.color.on_surface_variant)))
        addView(gap(28))
        addView(stepRow("1", LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.VERTICAL
            addView(text("On your PC, run", 16f))
            addView(gap(6)); addView(chip("zenvyd pair"))
        }))
        addView(gap(20))
        addView(stepRow("2", text("Scan the QR code it shows", 16f)))
        addView(gap(32))
        addView(button("Scan QR code", true) { scan() })
        addView(gap(12))
        pasteGroup = LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.VERTICAL; visibility = View.GONE
            addView(gap(4))
            linkInput = EditText(this@MainActivity).apply {
                hint = "https://…"; setHintTextColor(c(R.color.on_surface_variant)); setTextColor(c(R.color.on_surface)); textSize = 16f
                background = shape(Color.TRANSPARENT, 16, c(R.color.outline)); setPadding(dp(16), dp(16), dp(16), dp(16))
                inputType = InputType.TYPE_TEXT_VARIATION_URI or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS; maxLines = 3
            }
            addView(linkInput); addView(gap(12))
            addView(button("Connect", true) { handleLink(linkInput.text.toString()) })
        }
        lateinit var pasteToggle: Button
        pasteToggle = button("Paste the link instead", false) {
            pasteGroup.visibility = View.VISIBLE; pasteToggle.visibility = View.GONE; linkInput.requestFocus()
        }
        addView(pasteToggle)
        addView(pasteGroup)
    })

    private fun buildConnectingPanel(): View = scroll(column().apply {
        gravity = Gravity.CENTER
        addView(ProgressBar(this@MainActivity).apply {
            isIndeterminate = true; indeterminateTintList = ColorStateList.valueOf(c(R.color.primary))
            layoutParams = LinearLayout.LayoutParams(dp(44), dp(44))
        })
        addView(gap(24))
        addView(centered(text("Connecting", 22f, bold = true)))
        addView(gap(12))
        connectingHost = chip("")
        addView(connectingHost)
    })

    private fun buildErrorPanel(): View = scroll(column().apply {
        gravity = Gravity.CENTER
        errIcon = ImageView(this@MainActivity).apply { layoutParams = FrameLayout.LayoutParams(dp(36), dp(36), Gravity.CENTER) }
        addView(FrameLayout(this@MainActivity).apply {
            background = GradientDrawable().apply { shape = GradientDrawable.OVAL; setColor(c(R.color.surface_container_high)) }
            addView(errIcon)
            layoutParams = LinearLayout.LayoutParams(dp(88), dp(88))
        })
        addView(gap(24))
        errTitle = centered(text("", 24f, bold = true)); addView(errTitle)
        addView(gap(12))
        errHost = chip(""); addView(errHost)
        addView(gap(16))
        errBody = centered(text("", 15f, c(R.color.on_surface_variant))); addView(errBody)
        addView(gap(24))
        errPort = text("", 14f, c(R.color.on_surface_variant))
        errChecklist = LinearLayout(this@MainActivity).apply {
            orientation = LinearLayout.VERTICAL
            background = shape(c(R.color.surface_container), 20); setPadding(dp(20), dp(18), dp(20), dp(18))
            for (line in listOf<View>(text("zenvyd is running on the PC", 14f, c(R.color.on_surface_variant)), text("The phone is on the same Wi-Fi", 14f, c(R.color.on_surface_variant)), errPort)) {
                addView(row(dot(), line)); if (line !== errPort) addView(gap(10))
            }
        }
        addView(errChecklist)
        addView(gap(28))
        retryBtn = button("Try again", true) { connect() }; addView(retryBtn)
        addView(gap(12))
        forgetBtn = button("Forget this PC", false) { forgetPc() }; addView(forgetBtn)
    })

    private fun scan() {
        val opts = GmsBarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).enableAutoZoom().build()
        GmsBarcodeScanning.getClient(this, opts).startScan()
            .addOnSuccessListener { handleLink(it.rawValue) }
            .addOnFailureListener { Toast.makeText(this, "Scan failed or was cancelled. You can paste the link instead.", Toast.LENGTH_LONG).show() }
    }

}
