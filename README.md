# AMITEX-GUI

A native desktop GUI (Rust, [egui](https://github.com/emilk/egui)/[eframe](https://github.com/emilk/egui/tree/main/crates/eframe))
that wraps [AMITEX_FFTP](https://amitexfftp.github.io/AMITEX/), an FFT-based mechanics solver,
to run a specific micromechanics workflow without hand-editing XML or shell scripts.

## What it does today

Right now this app does **one thing**: elastic homogenization of a two-phase (or generally
material-ID-mapped) periodic microstructure.

Concretely: computing a material's effective (homogenized) 6x6 stiffness tensor requires 6 FFT
solves — one per canonical unit-strain direction in Voigt notation (`xx`, `yy`, `zz`, `xy`,
`xz`, `yz`). The app:

1. Generates the 6 loading XMLs itself (deterministic, so there's nothing to configure there),
2. Runs `mpirun amitex_fftp` once per direction against your material-ID VTK map, `mat.xml`, and
   algorithm XML,
3. Parses each case's `.std` output, assembles the 6x6 stiffness/compliance matrices, and derives
   `E`, `ν`, `G`, and the Zener anisotropy factor.

That's the whole feature set. There's no visualization of stress/strain fields, no run
history/config persistence, no per-material zone (`-nz`) support, and no other simulation modes
— the mode selector only has the one option.

## Requirements

You need to already have, separately:

- A built [`amitex_fftp`](https://amitexfftp.github.io/AMITEX/general/install.html) binary and
  an MPI `mpirun`.
- A material-ID VTK file (per-voxel material/phase assignment — AMITEX's `-nm` input).
- `mat.xml` (material properties) and an algorithm XML.

The app only orchestrates these; it doesn't build or bundle AMITEX_FFTP itself.

## Building / running

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

### Platform notes

- **macOS**: reading files under `Desktop`, `Documents`, `Downloads`, or iCloud Drive triggers a
  one-time system permission prompt per app (this is normal macOS behavior, not a bug).
- **AMITEX_PATH**: native material behaviors (an empty `Lib=""` in `mat.xml`) need
  `amitex_fftp`'s own `AMITEX_PATH` environment variable to resolve their shared library. The
  app derives this automatically from the `amitex_fftp` binary path you select, provided it
  follows AMITEX's documented `<root>/libAmitex/bin/amitex_fftp` layout.

### First run, if you downloaded a release binary

Downloaded binaries aren't signed with a paid platform certificate, so each OS shows a one-time
trust warning on first launch — this is normal, not a broken download:

- **macOS**: the release `.app` is ad-hoc signed (no paid Developer ID), so Gatekeeper shows
  "unidentified developer" rather than refusing outright. Right-click the app → **Open** (or
  System Settings → Privacy & Security → **Open Anyway**) once, then it launches normally.
- **Windows**: SmartScreen will show "Windows protected your PC" on first run of the unsigned
  `.exe`. Click **More info** → **Run anyway**.
- **Linux**: the extracted binary should already be executable; if not, `chmod +x AMITEX-GUI`
  before running it.

## Releases

Pushing a version tag (`vX.Y.Z`) triggers [`.github/workflows/release.yml`](.github/workflows/release.yml),
which builds release binaries for macOS (Apple Silicon), Linux (x86_64), and Windows (x86_64)
and attaches them to a GitHub Release.
