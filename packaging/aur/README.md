# AUR packaging

`PKGBUILD` for the **`xclaudeusage-bin`** AUR package. It installs the prebuilt
release binary (verifying its SHA-256), so Arch users get
`yay -S xclaudeusage-bin` (or `paru -S ...`) without compiling.

This directory is the *source of truth* for the package. Publishing to the AUR is
a manual step: it needs an AUR account with your SSH key registered at
<https://aur.archlinux.org/account>. The CI in this repo cannot push to the AUR.

## First publish

```bash
git clone ssh://aur@aur.archlinux.org/xclaudeusage-bin.git aur-pkg
cp packaging/aur/PKGBUILD packaging/aur/.SRCINFO aur-pkg/
cd aur-pkg
git add PKGBUILD .SRCINFO
git commit -m "xclaudeusage-bin 0.1.4"
git push
```

## On each new release

1. Bump `pkgver` in `PKGBUILD` (reset `pkgrel=1`).
2. Update `sha256sums_x86_64` / `sha256sums_aarch64` from the release
   `SHA256SUMS` (and `sha256sums` for the `LICENSE` if it changed).
3. Regenerate `.SRCINFO` from the `PKGBUILD`:
   ```bash
   makepkg --printsrcinfo > .SRCINFO
   ```
4. Test locally: `makepkg -si` (and ideally `namcap PKGBUILD`).
5. Commit and push to the AUR remote.

Always regenerate `.SRCINFO` with `makepkg --printsrcinfo` rather than editing it
by hand, so it can never drift from the `PKGBUILD`.
