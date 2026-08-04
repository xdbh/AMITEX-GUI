# AMITEX-GUI

A native desktop GUI (Rust, [egui](https://github.com/emilk/egui)/[eframe](https://github.com/emilk/egui/tree/main/crates/eframe))
that wraps [AMITEX_FFTP](https://amitexfftp.github.io/AMITEX/), an FFT-based mechanics solver,
to run a specific micromechanics workflow without hand-editing XML or shell scripts.

## Table of Contents

- [Current Capabilities](#current-capabilities)
- [Requirements](#requirements)
- [Quick Start](#quick-start)
  - [Linux / macOS](#linux--macos)
  - [Windows](#windows)
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

The app only orchestrates AMITEX_FFTP; it doesn't build or bundle it. You need, separately:

- A built [`amitex_fftp`](https://amitexfftp.github.io/AMITEX/general/install.html) binary
  (no native Windows build — Windows users run it inside [WSL](#windows)).
- `mpirun`:
  - macOS: `brew install open-mpi`
  - Linux / WSL: `sudo apt install openmpi-bin`
- A material-ID VTK file (per-voxel material/phase assignment — AMITEX's `-nm` input).
- `mat.xml` (material properties) and an algorithm XML.

**Linux / WSL only** — running a downloaded binary (as opposed to building it) also needs these
runtime libraries, which a minimal WSL Ubuntu image may not have preinstalled:

```
sudo apt install libgtk-3-0 libxcb-render0 libxcb-shape0 libxcb-xfixes0 libxkbcommon0 libgl1
```

If you skip this, the app will fail to launch with a "shared library not found" error naming
whichever one is missing.

## Quick Start

### Linux / macOS

1. Download the latest build for your OS from the [Releases page](../../releases).
2. **macOS**: right-click the app → **Open**. If Gatekeeper still blocks it, go to
   **System Settings → Privacy & Security** and click **Open Anyway** — first run only.
3. **Linux**: `chmod +x AMITEX-GUI` if it isn't already executable.

### Windows

`amitex_fftp` is Linux/MPI-only — run it, `mpirun`, and this GUI all inside WSL, using the Linux
instructions above.

1. Install WSL if needed: `wsl --install -d Ubuntu` (elevated PowerShell, then reboot if
   prompted).
2. Inside WSL, follow [Requirements](#requirements) and [Linux / macOS](#linux--macos) above.

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
which builds release binaries for macOS (Apple Silicon) and Linux (x86_64) and attaches them to
a GitHub Release. No Windows build — see [Windows](#windows) above.
