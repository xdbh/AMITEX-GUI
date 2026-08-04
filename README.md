# AMITEX-GUI

A native desktop GUI (Rust, [egui](https://github.com/emilk/egui)/[eframe](https://github.com/emilk/egui/tree/main/crates/eframe))
that wraps [AMITEX_FFTP](https://amitexfftp.github.io/AMITEX/), an FFT-based mechanics solver,
to run a specific micromechanics workflow without hand-editing XML or shell scripts.

## Table of Contents

- [Current Capabilities](#current-capabilities)
- [Requirements](#requirements)
- [Quick Start](#quick-start)
  - [Windows: run it via WSL](#windows-run-it-via-wsl)
- [Building from Source](#building-from-source)

## Current Capabilities

Right now this app does **one thing**: elastic homogenization of a two-phase (or generally
material-ID-mapped) periodic microstructure.

Concretely: computing a material's effective (homogenized) 6x6 stiffness tensor requires 6 FFT
solves — one per canonical unit-strain direction in Voigt notation (`xx`, `yy`, `zz`, `xy`,
`xz`, `yz`). The app:

1. Generates the 6 loading XMLs itself (deterministic, so there's nothing to configure there),
2. Runs `mpirun amitex_fftp` once per direction against your material-ID VTK map, `mat.xml`, and
   algorithm XML,
3. Parses each case's `.std` output, assembles the 6x6 stiffness/compliance matrices, and derives
   `E`, `ν`, `G`, and the Zener anisotropy factor,
4. Renders the material-ID VTK map as a rotatable 3D voxel mesh so you can sanity-check the
   input before running.

That's the whole feature set. There's no visualization of stress/strain fields, no run
history/config persistence, no per-material zone (`-nz`) support, and no other simulation modes
— the mode selector only has the one option.

## Requirements

You need to already have, separately:

- A built [`amitex_fftp`](https://amitexfftp.github.io/AMITEX/general/install.html) binary and
  an MPI `mpirun`. Follow AMITEX's own install docs for this:
  https://amitexfftp.github.io/AMITEX/general/install.html
- A material-ID VTK file (per-voxel material/phase assignment — AMITEX's `-nm` input).
- `mat.xml` (material properties) and an algorithm XML.

The app only orchestrates these; it doesn't build or bundle AMITEX_FFTP itself.

`amitex_fftp` has no native Windows build (it's Linux/MPI-only) — Windows users need
[WSL](#windows-run-it-via-wsl) to get it running.

## Quick Start

1. Download the latest build for your OS from the [Releases page](../../releases).
2. **macOS**: right-click the app → **Open**. If Gatekeeper still blocks it, go to
   **System Settings → Privacy & Security** and click **Open Anyway** — first run only.
3. **Windows**: on the SmartScreen prompt, click **More info → Run anyway** — first run only.
   (`amitex_fftp` itself doesn't run natively on Windows — see
   [WSL](#windows-run-it-via-wsl).)
4. **Linux**: `chmod +x AMITEX-GUI` if it isn't already executable.
5. Point the app at your `amitex_fftp` binary, material-ID VTK, `mat.xml`, and algorithm XML
   (see [Requirements](#requirements)) and run the homogenization.

### Windows: run it via WSL

`amitex_fftp` itself is Linux/MPI-native — there's no Windows build of the solver. The practical
path on Windows is to build and run everything (GUI, `amitex_fftp`, `mpirun`) inside WSL2,
rather than running the native Windows `.exe` and trying to point it at a solver binary that
lives in a separate WSL filesystem.

**1. Install WSL2 with GUI support (WSLg).** You need Windows 11, or Windows 10 21H2+ with WSLg
backported. From an elevated PowerShell:

```
wsl --install -d Ubuntu
```

This installs WSL2, an Ubuntu distro, and WSLg (bundled with WSL2 since ~2021), which gives you
Wayland/X11 forwarding out of the box — no manual X server (VcXsrv, Xming, etc.) needed. Reboot
if prompted, then launch "Ubuntu" from the Start menu to finish first-time user setup.

Confirm GUI passthrough works before going further:

```
sudo apt update && sudo apt install -y x11-apps
xeyes
```

If a window with eyes that track your cursor appears, WSLg is working. If nothing appears, fix
WSLg first (Windows Update, or `wsl --update` from PowerShell) — nothing below will render
without it.

**2. Install `amitex_fftp` and its dependencies inside WSL,** following AMITEX's own install
docs: https://amitexfftp.github.io/AMITEX/general/install.html. Do this inside the WSL
filesystem (e.g. `~/amitex`, not `/mnt/c/...`) — building on the Windows-side 9p-mounted
filesystem is noticeably slower and some build scripts trip on the permission/symlink
differences. `mpirun` is installed as part of that (`libopenmpi-dev openmpi-bin` or similar,
per AMITEX's docs).

**3. Get AMITEX-GUI running inside WSL** — either download the Linux release binary from the
[Releases page](../../releases) and `chmod +x` it, or [build from source](#building-from-source)
inside WSL the same way you would on native Linux.

The materials viewer renders its 3D voxel mesh through OpenGL (egui's Glow backend). WSLg
usually gives you the host GPU via DXGI/D3D12 passthrough; if that's unavailable it falls back
to `llvmpipe` (software rendering via Mesa), which works but is visibly slower when rotating
large meshes. Check which one you're getting with:

```
sudo apt install -y mesa-utils
glxinfo | grep "OpenGL renderer"
```

`llvmpipe` in that output means software rendering — the app still works, just don't expect the
3D view to be smooth on large voxel grids. If `glxinfo` shows neither your actual GPU nor
`llvmpipe`, update your Windows GPU driver (WSLg's GPU passthrough is exposed through it).

**File paths and `mpirun` notes:**

- **Windows files from WSL**: your `C:\` drive is at `/mnt/c/...`. Point the GUI's file pickers
  there if your VTK/XML inputs live on the Windows side, though for speed it's better to keep
  them in the WSL filesystem (`~/...`).
- **WSL files from Windows**: `\\wsl$\Ubuntu\home\<user>\...` in Explorer, if you need to move
  files the other way.
- **`mpirun` refusing to run as root**: if your WSL user ended up as `root` (uncommon with
  `wsl --install`, more common in minimal/container-style setups), OpenMPI refuses to launch by
  default. Fix the user rather than passing `--allow-run-as-root` — the GUI always invokes
  `mpirun` without that flag.

## Building from Source

```
cargo build --release
```

On macOS, `packaging/Info.plist` can be used to wrap the release binary into a real `.app`
bundle (lets it launch from Finder/Dock without a terminal, and gives it its own identity for
macOS's per-app folder permissions):

```
mkdir -p AMITEX-GUI.app/Contents/MacOS
cp target/release/AMITEX-GUI AMITEX-GUI.app/Contents/MacOS/AMITEX-GUI
cp packaging/Info.plist AMITEX-GUI.app/Contents/Info.plist
```

On Linux (and WSL), building needs a few native dev packages first — see the
`Install Linux build dependencies` step in
[`.github/workflows/release.yml`](.github/workflows/release.yml) for the exact list.

**AMITEX_PATH**: native material behaviors (an empty `Lib=""` in `mat.xml`) need
`amitex_fftp`'s own `AMITEX_PATH` environment variable to resolve their shared library. The app
derives this automatically from the `amitex_fftp` binary path you select, provided it follows
AMITEX's documented `<root>/libAmitex/bin/amitex_fftp` layout.

### Releases

Pushing a version tag (`vX.Y.Z`) triggers [`.github/workflows/release.yml`](.github/workflows/release.yml),
which builds release binaries for macOS (Apple Silicon), Linux (x86_64), and Windows (x86_64)
and attaches them to a GitHub Release.
