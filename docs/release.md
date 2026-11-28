# Releasing xctx as a signed, notarized DMG

`mise run dmg` builds `xctx.app`, packs it into `xctx.dmg`, and signs and notarizes it when told how.

## One-time setup

1. **Certificate.** An Apple Developer Program membership is needed. In the developer portal
   (Certificates, Identifiers & Profiles) create a **Developer ID Application** certificate from a certificate signing
   request, download the `.cer`, and double-click it so it lands in the login keychain next to its private key.
   `security find-identity -v -p codesigning` must then list a `Developer ID Application: <name> (<team id>)` identity.
   An `Apple Development` identity cannot sign for distribution outside the Mac App Store.
2. **Notary credentials.** Store them once in the keychain under a profile name:

   ```sh
   xcrun notarytool store-credentials xctx-notary \
     --apple-id <apple id> --team-id <team id> --password <app-specific password>
   ```

   The app-specific password is made at appleid.apple.com under Sign-In and Security.

## Each release

```sh
XCTX_SIGN_IDENTITY="Developer ID Application: <name> (<team id>)" \
XCTX_NOTARY_PROFILE=xctx-notary \
mise run dmg
```

- Without `XCTX_SIGN_IDENTITY`, the app is signed ad hoc and the dmg is unsigned (local use only).
- Without `XCTX_NOTARY_PROFILE`, the dmg is signed but not notarized, so Gatekeeper still warns on other Macs.
- `mise run bundle` takes the same `XCTX_SIGN_IDENTITY` and signs the app with the hardened runtime and a secure
  timestamp, as notarization requires.
