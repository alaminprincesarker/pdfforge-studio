# PDFForge Studio — GitHub Release & Pages Publishing Guide

- **GitHub Repository**: [https://github.com/alaminprincesarker/pdfforge-studio](https://github.com/alaminprincesarker/pdfforge-studio)
- **GitHub Pages Website**: [https://alaminprincesarker.github.io/pdfforge-studio/](https://alaminprincesarker.github.io/pdfforge-studio/)
- **Releases Page**: [https://github.com/alaminprincesarker/pdfforge-studio/releases](https://github.com/alaminprincesarker/pdfforge-studio/releases)
- **In-App Update Manifest URL**: `https://alaminprincesarker.github.io/pdfforge-studio/update.json`

---

## Step 1 — Push Updates to GitHub (`main`)
Run in PowerShell inside `c:\Users\LENOVO\AndroidStudioProjects\PDF Open and Edit`:

```powershell
git add .
git commit -m "Update GitHub Pages website & live repo links"
git push origin main
```

---

## Step 2 — Upload Installers to GitHub Release (`v1.0.0`)
Because large binaries (`.exe` / `.zip` / `target/`) are excluded by `.gitignore`, attach them directly to **GitHub Releases**:

1. Open [https://github.com/alaminprincesarker/pdfforge-studio/releases/new](https://github.com/alaminprincesarker/pdfforge-studio/releases/new).
2. Under **Choose a tag**, enter **`v1.0.0`** (create tag on publish).
3. Set title: **`PDFForge Studio v1.0.0 (Native Rust Edition)`**.
4. Drag and drop these 3 files from `release\`:
   - `release\PDFForge-Setup-1.0.0-Windows-x64.exe`
   - `release\PDFForge-1.0.0-Windows-x64-Portable.zip`
   - `release\PDFForge-1.0.0-macOS-Installer.zip`
5. Click **Publish release**.

---

## Step 3 — Enable GitHub Pages Website (`docs/index.html`)
1. Open [https://github.com/alaminprincesarker/pdfforge-studio/settings/pages](https://github.com/alaminprincesarker/pdfforge-studio/settings/pages).
2. Under **Build and deployment &rarr; Source**, select **Deploy from a branch**.
3. Choose Branch **`main`** and folder **`/docs`**, then click **Save**.
4. Wait ~30–60 seconds and open:
   **[https://alaminprincesarker.github.io/pdfforge-studio/](https://alaminprincesarker.github.io/pdfforge-studio/)**

All Download buttons on the website point directly to your `v1.0.0` release assets, and `Help -> Check for Updates...` inside PDFForge Studio can check `https://alaminprincesarker.github.io/pdfforge-studio/update.json` or `https://api.github.com/repos/alaminprincesarker/pdfforge-studio/releases/latest`.

---

## Step 4 — Remove SmartScreen (`Unknown publisher`) & macOS Gatekeeper Warnings Worldwide
See [SIGNING_AND_TRUST_SETUP.md](SIGNING_AND_TRUST_SETUP.md) for configuring **Azure Trusted Signing / SignPath.io / PFX** (Windows) and **Apple Developer ID Notarization** (macOS) in `.github/workflows/release.yml`.
