# Upload the macOS DMG from Windows (no Mac needed)

The ready-to-upload Apple Silicon DMG is `release-assets/Pronto_0.8.2_aarch64.dmg` in this workspace. The DMG is intentionally excluded from Git commits (`release-assets/*.dmg` is gitignored); upload it as a GitHub Release asset. Only code, docs, scripts, and the `.sha256` are committed.

This is a Windows-only flow: no Mac or macOS build tools are needed on this laptop for the upload. Future DMG refreshes work the same way -- rebuild once (local Mac or CI `macos-15` job), copy the new DMG + `.sha256` into `release-assets/`, update the script paths/tag, and re-run the publish script.

1. Install [Git for Windows](https://git-scm.com/download/win), [Git LFS](https://git-lfs.com/), and [GitHub CLI](https://cli.github.com/), then run `gh auth login` in PowerShell. Make sure `gh` can access `Jashith127/pronto` (`gh repo view Jashith127/pronto`).
2. Merge the Mac port into this checkout (Windows + Mac code already share one repo with separated platform paths: `src-tauri/tauri.conf.json` + `installer-hooks.nsh` for Windows, `src-tauri/tauri.macos.conf.json` + `src-tauri/src/platform/macos/` for Mac), then push `main`:

   ```powershell
   git add .github docs scripts src-tauri ARCHITECTURE.md README.md RELEASE_NOTES.md THIRD_PARTY_NOTICES.md .gitignore ui
   git commit -m 'Port Pronto to macOS (Pronto for Mac)'
   git push origin main
   ```

   The existing Git history uses LFS for Windows speech binaries and the model. If Git LFS reports a missing object, run `git lfs pull` before pushing. Do not use `GIT_LFS_SKIP_PUSH=1`, which would leave broken binary pointers.

3. Verify and upload the DMG as a MAC-only **draft** release (tag `v0.8.2-macos`, separate from the Windows `v0.8.2` release):

   ```powershell
   .\scripts\publish-macos-release.ps1 -Repo Jashith127/pronto -Tag v0.8.2-macos
   ```

   The script checks the DMG against its SHA-256 file and creates a draft `v0.8.2-macos` release containing only the DMG. Review the notes and asset on GitHub, then publish the draft when ready.

This local DMG is ad hoc signed and not notarized. It may show a macOS Gatekeeper warning. Normal public distribution requires rebuilding on a Mac with a Developer ID certificate and Apple notarization credentials (`scripts/build-macos.sh signed`).
