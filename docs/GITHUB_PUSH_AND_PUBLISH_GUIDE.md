# How to Push PDFForge Studio to GitHub, Publish Releases, and Launch Your Website

> This workspace has been prepared with [`.gitignore`](../.gitignore), [`LICENSE`](../LICENSE) (MIT), [`docs/index.html`](./index.html) (your open-source project website), and [`docs/update.json`](./update.json). Because `/target/` (~1.4 GB) and large `.exe` / `.zip` installers are excluded by `.gitignore`, pushing source code stays small and fast while installers live properly in **GitHub Releases**.

---

## Step 1 — Create an Empty Repository on GitHub
1. Sign in to [https://github.com](https://github.com) and click **+ &rarr; New repository**.
2. Set a **Repository name** (for example `pdfforge-studio` or `PDFForge`).
3. Keep it **Public** (for open source & free GitHub Pages hosting).
4. **Do NOT** check *Add a README file*, *.gitignore*, or *Choose a license* (those files already exist locally).
5. Click **Create repository**, and copy the repository HTTPS URL (e.g. `https://github.com/<YOUR-USERNAME>/pdfforge-studio.git`).

---

## Step 2 — Initialize Git & Push Source Code from This Folder
Open PowerShell inside `c:\Users\LENOVO\AndroidStudioProjects\PDF Open and Edit` and run:

```powershell
# 1. Initialize Git on the main branch
git init -b main

# 2. Verify only source, assets, docs, and metadata are staged (target/ and large .exe/.zip are ignored)
git status

# 3. Stage all tracked files and commit
git add .
git commit -m "Release v1.0.0: PDFForge Studio (Native Rust PDF Reader & Editor)"

# 4. Connect your GitHub repository and push
git remote add origin https://github.com/<YOUR-USERNAME>/<YOUR-REPO>.git
git push -u origin main
```

> **Note:** Git will ask you to sign in to GitHub (via browser OAuth or a Personal Access Token) on your first `git push`.

---

## Step 3 — Create a GitHub Release (`v1.0.0`) & Upload Installers
Uploading your compiled binaries to **GitHub Releases** keeps git history fast and makes all Download links on your website work automatically:

1. On your GitHub repository page, click **Releases &rarr; Create a new release** (right sidebar).
2. Click **Choose a tag**, type **`v1.0.0`**, and choose **Create new tag: v1.0.0 on publish**.
3. Set **Release title** to: `PDFForge Studio v1.0.0 (Native Rust Edition)`.
4. Drag and drop these exact files from your local `release\` folder into **Attach binaries by dropping them here**:
   - `release\PDFForge-Setup-1.0.0-Windows-x64.exe`
   - `release\PDFForge-1.0.0-Windows-x64-Portable.zip`
   - `release\PDFForge-1.0.0-macOS-Installer.zip`
   *(Keep exact filenames so the website download links resolve automatically!)*
5. Click **Publish release**.

---

## Step 4 — Publish Your Open-Source Website (`docs/index.html`) on GitHub Pages
1. On your GitHub repository, open **Settings &rarr; Pages** (left sidebar under *Code and automation*).
2. Under **Build and deployment &rarr; Source**, leave **Deploy from a branch** selected.
3. Under **Branch**, select **`main`** and folder **`/docs`**, then click **Save**.
4. Wait ~30–60 seconds and refresh the page — your live website URL will appear at the top:
   ```text
   https://<YOUR-USERNAME>.github.io/<YOUR-REPO>/
   ```
5. **`docs/index.html` automatically detects your GitHub username and repository name** from its live URL (`<username>.github.io/<repo>/`) and automatically wires:
   - All **Download** buttons directly to `https://github.com/<YOUR-USERNAME>/<YOUR-REPO>/releases/latest/download/...`
   - All **View Source / License / Docs** links to your repo
   - The **Internal Update Manifest URL** (`https://<YOUR-USERNAME>.github.io/<YOUR-REPO>/update.json`) which you can paste into **Help &rarr; Check for Updates...** inside PDFForge Studio.

---

## Step 5 — Pushing Future Updates
Whenever you make code improvements and rebuild `release\`:
```powershell
git add .
git commit -m "Update v1.0.1: description of changes"
git push
```
Then bump `"version"` in `Cargo.toml`, `src-rust/app.rc`, and `docs/update.json`, rebuild (`cargo build --release`), create a new GitHub Release tag (`v1.0.1`), and upload the updated `PDFForge-Setup-...exe`. Users with PDFForge installed can update directly inside the app via **Help &rarr; Check for Updates...**.

