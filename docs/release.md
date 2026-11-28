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

## Releases from GitHub Actions

`.github/workflows/release.yml` builds the same signed, notarized dmg on a `macos-15` runner when a `v*` tag is
pushed (`git tag v0.1.0 && git push origin v0.1.0`) and attaches it to a GitHub release. A manual run
(`gh workflow run release.yml`) with no `tag` input builds the dmg and keeps it as a workflow artifact only.

It reads five repository secrets:

| Secret | Contents |
|---|---|
| `DEVELOPER_ID_P12_BASE64` | the Developer ID Application certificate and its private key, as a base64-encoded `.p12` |
| `P12_PASSWORD` | the `.p12` export password |
| `NOTARY_APPLE_ID` | the Apple ID that owns the app-specific password |
| `NOTARY_TEAM_ID` | the Developer team id |
| `NOTARY_PASSWORD` | an app-specific password for that Apple ID |

The job imports the certificate into a temporary keychain, stores the notary profile there, runs `mise run dmg`, and
deletes the keychain afterwards. Secrets never reach pull requests, because the workflow does not run on them.

To rotate the notary password, see "Rotating the notary password" in the README.

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
