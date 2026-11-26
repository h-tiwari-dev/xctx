---
name: xctx-browser
description: Open, show or test the space's web app in xctx's own browser pane beside you. Use when the user asks to open a browser, see or preview the site, check a page, or when you start a dev server or change UI they will want to look at. Never open an outside browser.
---

# The browser beside you in xctx

You run inside an xctx space. It has its own live browser, a pane in the plugin column on the right of the
window, beside your pane. The user watches it and so can you. Use it instead of any other browser: never run
`open http://...`, never start Chrome or Safari, never use another browser tool.

`$XCTX_SPACE` names your space and `$PORT` is the port its app serves on. From the Parent Agent on Home there is
no `$XCTX_SPACE`: pass the space's slug from `xctx-ctl space.list` instead.

## When to open it

- The user asks to open a browser, see the site, preview a page or check how something looks.
- You started the app's dev server and it is up.
- You changed something visible and the user will want to see it.

## How

1. Make sure the app is up on `$PORT`. If no dev server is running, start it in a terminal with the repo's
   own script on that port (for example `bun run dev --port "$PORT"` or `npm run dev -- --port "$PORT"`), and
   wait until it prints that it is ready.
2. See whether the space has a browser already:

   ```sh
   xctx-ctl plugin.list space="$XCTX_SPACE"
   ```

   Mount one only when `browser` is not mounted there; a second browser pane cannot show a page.

   ```sh
   xctx-ctl plugin.mount space="$XCTX_SPACE" plugin=browser
   ```

3. Open the page you mean, a path or a full `http://localhost:$PORT/...` URL:

   ```sh
   xctx-ctl plugin.call space="$XCTX_SPACE" plugin=browser tool=page.open args:='{"path":"/"}'
   ```

4. Tell the user it is open beside you and what they are looking at.

## Checking the page yourself

The same pane's tools let you look at what the user sees:

- `tool=page.text` reads the page's text, `tool=page.screenshot` saves a screenshot.
- `tool=page.console args:='{"errors_only":true}'` lists console errors and failed requests.
- `tool=page.click args:='{"selector":"..."}'` and `tool=page.type` drive it.

These tools only open and read the space's own app (`localhost:$PORT`). The user may browse anywhere in the
pane; when it shows another site, the tools refuse to read it. Open your page again with `page.open`.

## An API instead of a page

For a backend with no page to show, mount the API client beside you instead:
`xctx-ctl plugin.mount space="$XCTX_SPACE" plugin=api-client`.
