# Zenvy Browser Bridge

Optional extension for Chromium browsers (Brave, Chrome, Edge). It lets the Zenvy phone app show a browser window's tabs on the Desk screen and switch, close or open tabs.

**Privacy:** it sends only each tab's *title* and *site name* (e.g. `docs.rs`) to the Zenvy agent on this computer over loopback (`127.0.0.1`), protected by a token. Full addresses, page content, cookies and history never leave the browser. Incognito windows are invisible to it unless you allow the extension in incognito.

Install: run `zenvyd browser setup`, then in `brave://extensions` (or `chrome://extensions`) turn on Developer mode, choose **Load unpacked**, pick this folder, and paste the token in the extension's options.
