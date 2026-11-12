# Solving Windows SmartScreen (`Unknown publisher`) & macOS Gatekeeper

## Why `Windows protected your PC` (`Publisher: Unknown publisher`) Appears
Microsoft Defender SmartScreen checks every downloaded `.exe` against two signals **before Windows starts it**:
1. A valid **Authenticode Code Signing Certificate** issued by a publicly trusted Certificate Authority (or Microsoft Trusted Signing), which fills in the **Publisher** name (`Md. Alamin`) instead of `Unknown publisher`.
2. SmartScreen **reputation** (automatic on Azure Trusted Signing / EV Certificates, or accumulated over downloads on standard OV certificates).

> **Important OS Security Rule**: No unsigned or self-signed binary can bypass Windows SmartScreen on external PCs by itself — SmartScreen evaluates downloads *before* the executable runs. Only a publicly trusted Code Signing CA (Windows) or Apple Developer ID + Notarization (macOS) eliminates the prompt for every user worldwide.

---

## What Has Been Implemented in This Repo

1. **Windows Clean Install & Zone.Identifier Stripping** ([`src-rust/installer.rs`](../src-rust/installer.rs))
   - During install and in-app GitHub auto-updates, PDFForge automatically strips `:Zone.Identifier` (Mark-of-the-Web) streams from `PDFForge.exe`, `Uninstall-PDFForge.exe`, and `WebView2Loader.dll` so installed executables never carry browser quarantine warnings.
2. **Windows Authenticode Code Signing Script** ([`scripts/sign-windows.ps1`](../scripts/sign-windows.ps1))
   - Signs all Windows binaries with SHA-256 Authenticode + RFC 3161 DigiCert timestamping.
   - Supports any commercial/CA `.pfx` (`WINDOWS_PFX_PATH` / `WINDOWS_PFX_PASSWORD`), or creates & trusts `Md. Alamin (PDFForge Studio)` locally on your development/team PCs (`Md-Alamin-PDFForge-Publisher.cer`).
3. **macOS Hardened Runtime, WebKit Entitlements & Quarantine Cleanup** ([`release/macos/entitlements.plist`](../release/macos/entitlements.plist), [`release/macos/Install-PDFForge-macOS.command`](../release/macos/Install-PDFForge-macOS.command))
   - Clears `com.apple.quarantine` attributes (`xattr -cr`) on install.
   - Signs `PDFForge.app` with Hardened Runtime + required WKWebView entitlements (`--options runtime --entitlements`), using your `Developer ID Application` certificate when available or Apple Silicon compatible ad-hoc signing (`--sign -`) so M1–M4 Macs never reject execution.
4. **Automated CI Signing + Apple Notarization** ([`.github/workflows/release.yml`](../.github/workflows/release.yml))
   - Builds **Windows x64** + **macOS Universal (`aarch64` + `x86_64`)** `.app`, `.dmg`, and `.zip`.

---

## How to Remove Warnings for Everyone Worldwide (Free / Official Options)

### Option A — Windows (Removes `Unknown publisher` & SmartScreen Worldwide)
- **Free for Open Source (`pdfforge-studio`)**: Apply at **[SignPath.io Foundation](https://signpath.org/)** (free Authenticode code signing for OSS GitHub repos).
- **Microsoft Official**: **[Azure Trusted Signing](https://learn.microsoft.com/en-us/azure/trusted-signing/)** (~$9.99/month, removes SmartScreen immediately).
- Add these GitHub repository **Actions Secrets** (`Settings -> Secrets and variables -> Actions`):
  - For Azure Trusted Signing: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TS_ENDPOINT`, `AZURE_TS_ACCOUNT`, `AZURE_TS_PROFILE`
  - Or for a PFX Code Signing certificate: `WINDOWS_PFX_BASE64`, `WINDOWS_PFX_PASSWORD`

### Option B — macOS (Removes Gatekeeper `"developer cannot be verified"` Worldwide)
1. Enroll in the **Apple Developer Program** (allows Developer ID Application certificates + Notarization).
2. Add these GitHub **Actions Secrets**:
   - `APPLE_CERT_BASE64` (exported Developer ID Application `.p12`, base64 encoded)
   - `APPLE_CERT_PASSWORD`
   - `APPLE_SIGNING_IDENTITY` (e.g. `Developer ID Application: Md. Alamin (TEAMID)`)
   - `APPLE_ID` (Apple account email)
   - `APPLE_TEAM_ID` (10-char Team ID)
   - `APPLE_APP_PASSWORD` (app-specific password from `appleid.apple.com`)
3. Push a version tag (`git tag v1.0.1 && git push origin v1.0.1`) and GitHub Actions will automatically build, sign, notarize, staple `.dmg` + `.zip`, and publish the Release.

---

## Current User Workaround (Before Adding CA/Apple Secrets)
- **Windows SmartScreen**: Click **More info** -> **Run anyway**, OR run in PowerShell:
  ```powershell
  Unblock-File "$env:USERPROFILE\Downloads\PDFForge-Setup-1.0.0-Windows-x64.exe"
  ```
- **macOS Gatekeeper**: Double-click `Install-PDFForge-macOS.command` (which strips quarantine and signs locally for Apple Silicon), OR run:
  ```bash
  xattr -dr com.apple.quarantine "/Applications/PDFForge.app"
  ```

