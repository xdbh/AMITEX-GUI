use crate::pipeline::loads::{case_label, generate_load_xml};
use crate::postproc::vonmises::von_mises;
use crate::postproc::vtkio::{read_vtk_cell_scalars, write_vtk_cell_scalars, VtkGrid};
use anyhow::Context;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

/// Everything needed to launch the 6-case elastic-homogenization run.
pub(crate) struct PipelineConfig {
    /// Directory the 6 `load_<xx|yy|zz|xy|xz|yz>/` case directories are created under.
    pub(crate) run_dir: PathBuf,
    /// The AMITEX material-ID map (`-nm`) — same file for all 6 cases, referenced by
    /// absolute path rather than copied into each case directory.
    pub(crate) material_id_vtk: PathBuf,
    /// The AMITEX zone-ID map (`-nz`) — zones *within* a material (e.g. per-grain
    /// orientation). Optional: AMITEX assumes one zone per material if omitted.
    pub(crate) zone_id_vtk: Option<PathBuf>,
    pub(crate) mat_xml: PathBuf,
    pub(crate) algo_xml: PathBuf,
    /// Resolved by sourcing `ConfigTab::env_script` (see `resolve_amitex_binary`), not
    /// browsed-to directly — the binary's location is whatever that install's own
    /// env-setup script says it is.
    pub(crate) amitex_path: PathBuf,
    pub(crate) mpirun_path: PathBuf,
    /// The full environment captured by sourcing the user's `env_amitex.sh`-equivalent
    /// script (see `source_env_script`), applied verbatim to the `mpirun` child. Covers
    /// whatever that script sets up — `AMITEX_PATH`, `PATH`, `LD_LIBRARY_PATH` for
    /// FFTW/OpenMPI/MFront shared libs, etc. — rather than this GUI trying to guess a single
    /// `AMITEX_PATH` value from the binary's directory layout.
    pub(crate) env_vars: Vec<(String, String)>,
    /// Raw extra `amitex_fftp` CLI tokens appended verbatim after the standard flags — an
    /// escape hatch for AMITEX options this GUI has no dedicated field for (present or
    /// future), so using them doesn't need a GUI/Rust code change.
    pub(crate) extra_args: Vec<String>,
}

#[derive(Clone)]
struct CaseRun {
    label: String,
    dir: PathBuf,
}

pub(crate) enum RunEvent {
    CaseStarted { case: String },
    CaseOutput { case: String, line: String },
    CaseFinished { case: String, success: bool },
    /// The `.std` path for each of the 6 finished cases, in Voigt order — sent once,
    /// only when all 6 finished successfully, ready for `postproc::moduli`.
    AllFinished { std_paths: [PathBuf; 6] },
    Fatal(String),
}

fn prepare_case(config: &PipelineConfig, index: usize) -> anyhow::Result<CaseRun> {
    let label = case_label(index);
    let dir = config.run_dir.join(&label);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // mat.xml/algo.xml aren't copied in: like material_id_vtk, they're identical across all
    // 6 cases, so amitex_fftp is pointed at the shared file directly via absolute path. Only
    // char.xml actually differs per case, so it alone is written into the case directory.
    std::fs::write(dir.join("char.xml"), generate_load_xml(index))
        .with_context(|| format!("writing char.xml into {}", dir.display()))?;
    Ok(CaseRun { label, dir })
}

/// Runs the user's `env_amitex.sh`-equivalent script through a shell's `.` (source) command
/// and captures the resulting environment — mirroring what the legacy `run.sh` did
/// (`source $CODE_PATH/env_amitex.sh`) before invoking `amitex_fftp`. This is the correct way
/// to pick up whatever `PATH`/`LD_LIBRARY_PATH`/`AMITEX_PATH`/etc. that script establishes
/// (for the compiler, OpenMPI, FFTW, MFront libs it was built against), rather than this GUI
/// trying to reverse-engineer a single `AMITEX_PATH` value from the binary's own directory
/// layout, which only covers one specific install convention and says nothing about
/// `LD_LIBRARY_PATH`. `$1`/`sh` avoid needing to shell-escape `script`'s path into a `-c`
/// string.
pub(crate) fn source_env_script(script: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(". \"$1\" && env")
        .arg("sh")
        .arg(script)
        .output()
        .with_context(|| format!("running a shell to source {}", script.display()))?;
    if !output.status.success() {
        anyhow::bail!(
            "sourcing {} failed: {}",
            script.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect())
}

/// Locates `amitex_fftp` from an environment captured by `source_env_script`: prefers
/// AMITEX's own documented layout (`$AMITEX_PATH/libAmitex/bin/amitex_fftp`) if the script set
/// `AMITEX_PATH`, otherwise searches `PATH` the way a shell would — since a script that just
/// prepends the AMITEX `bin` directory to `PATH`, without setting `AMITEX_PATH` itself, is
/// just as valid.
pub(crate) fn resolve_amitex_binary(env_vars: &[(String, String)]) -> Option<PathBuf> {
    if let Some((_, amitex_path)) = env_vars.iter().find(|(k, _)| k == "AMITEX_PATH") {
        let candidate = Path::new(amitex_path).join("libAmitex/bin/amitex_fftp");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let (_, path_var) = env_vars.iter().find(|(k, _)| k == "PATH")?;
    std::env::split_paths(path_var).map(|dir| dir.join("amitex_fftp")).find(|p| p.is_file())
}

/// Best-effort search for an MPI launcher, for the "Auto" button next to the `mpirun` field.
/// Checks a handful of well-known fixed install locations first — a GUI app launched via
/// Finder/an IDE (rather than an interactive shell) often has a much smaller `PATH` than the
/// shell that built it, so `/opt/homebrew/bin` etc. may not be on it even when the binary is
/// right there — then falls back to searching this process's own `PATH`. Returns `None`
/// rather than guessing wrong; the manual "Browse…" button is always still available.
///
/// Confirmed to work against real installs on macOS (Homebrew) and should hold on Linux
/// (system package managers install to the same handful of standard prefixes), but is
/// untested on Windows — MPI there is more commonly `mpiexec.exe` from a specific vendor
/// install (Microsoft MPI/Intel MPI) rather than something living on `PATH` by convention, so
/// the fixed-location guesses for Windows are a starting point, not a verified fix.
pub(crate) fn find_mpirun() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) { &["mpiexec.exe", "mpirun.exe"] } else { &["mpirun", "mpiexec"] };

    let fixed_dirs: &[&str] = if cfg!(target_os = "macos") {
        &["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"]
    } else if cfg!(target_os = "linux") {
        &["/usr/bin", "/usr/local/bin", "/opt/openmpi/bin"]
    } else if cfg!(windows) {
        &[
            "C:\\Program Files\\Microsoft MPI\\Bin",
            "C:\\Program Files (x86)\\Microsoft MPI\\Bin",
        ]
    } else {
        &[]
    };
    for dir in fixed_dirs {
        for name in names {
            let candidate = Path::new(dir).join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    let path_var = std::env::var("PATH").ok()?;
    std::env::split_paths(&path_var)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|p| p.is_file())
}

fn std_path(case: &CaseRun) -> PathBuf {
    case.dir.join(format!("_{}.std", case.label))
}

/// Where this case's `mpirun`/`amitex_fftp` stdout+stderr are mirrored to disk. Distinct
/// from AMITEX's own `_<label>.log` (its internal Fortran log), which doesn't capture
/// MPI runtime errors or crash output (e.g. a segfault) the way stderr does.
fn command_log_path(case: &CaseRun) -> PathBuf {
    case.dir.join(format!("_{}_command.log", case.label))
}

/// Streams a child's output pipe line-by-line back over `tx`, tagged with `case`, and
/// mirrors each line (prefixed with `stream_name`) to `log_file` so a crash still leaves a
/// trace on disk after the GUI's in-memory log is gone.
fn stream_output(
    pipe: impl std::io::Read,
    case: String,
    stream_name: &'static str,
    log_file: Option<Arc<Mutex<std::fs::File>>>,
    tx: Sender<RunEvent>,
) {
    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
        if let Some(file) = &log_file {
            if let Ok(mut file) = file.lock() {
                let _ = writeln!(file, "[{stream_name}] {line}");
            }
        }
        if tx.send(RunEvent::CaseOutput { case: case.clone(), line }).is_err() {
            return;
        }
    }
}

/// Simple per-case postprocessing: von Mises stress from the case's 6
/// `_<label>_sig{1..6}_1.vtk` outputs, written to `_<label>_sigVM_1.vtk`. Ported from
/// `stress6toVM.py`. Not every run configuration writes stress VTKs, so a missing file
/// is reported as a log line rather than a failure.
fn postprocess_case(case: &CaseRun, tx: &Sender<RunEvent>) {
    let sig_paths: Vec<PathBuf> = (1..=6)
        .map(|i| case.dir.join(format!("_{}_sig{i}_1.vtk", case.label)))
        .collect();

    if !sig_paths.iter().all(|p| p.exists()) {
        return;
    }

    let result = (|| -> anyhow::Result<()> {
        let grids: Vec<VtkGrid> = sig_paths
            .iter()
            .map(|p| read_vtk_cell_scalars(p))
            .collect::<anyhow::Result<_>>()?;
        let sig: [Vec<f64>; 6] = std::array::from_fn(|i| grids[i].data.clone());
        let out = VtkGrid {
            nx: grids[0].nx,
            ny: grids[0].ny,
            nz: grids[0].nz,
            dx: grids[0].dx,
            dy: grids[0].dy,
            dz: grids[0].dz,
            varname: "sig_VM".to_string(),
            datatype: grids[0].datatype.clone(),
            data: von_mises(&sig),
        };
        write_vtk_cell_scalars(&out, &case.dir.join(format!("_{}_sigVM_1.vtk", case.label)))
    })();

    let line = match result {
        Ok(()) => "von Mises stress computed (_sigVM_1.vtk)".to_string(),
        Err(err) => format!("von Mises post-processing skipped: {err}"),
    };
    let _ = tx.send(RunEvent::CaseOutput { case: case.label.clone(), line });
}

/// Checks `path` exists before it's handed to `Command::new`/spawned as an arg. Without this,
/// a bad `mpirun`/`amitex_fftp` path fails identically for all 6 cases with the OS's bare
/// `No such file or directory (os error 2)` — which doesn't say *which* path was wrong — and
/// only the last case's copy of that message survives in the GUI (`state.fatal` keeps just the
/// most recent `Fatal` event), so five of the six failures give no information at all.
fn check_binary_exists(path: &Path, label: &str) -> anyhow::Result<()> {
    if !path.is_file() {
        anyhow::bail!("{label} not found: {}", path.display());
    }
    Ok(())
}

/// Actually attempts to open `path`, rather than just checking `Path::exists`/permission bits:
/// on macOS a file can exist and look readable by its Unix mode yet still be denied at open()
/// time by Gatekeeper quarantine, a stray ACL, or the file-system sandbox (the exact failure
/// mode that motivated this check) — the only reliable test is to actually try.
fn check_readable(path: &Path) -> Result<(), String> {
    std::fs::File::open(path)
        .map(|_| ())
        .map_err(|err| format!("not readable ({err})"))
}

/// Same as `check_readable`, plus (on Unix) the executable bit, since a readable-but-not-
/// executable `amitex_fftp`/`mpirun` fails at spawn time with a less obvious error.
fn check_executable(path: &Path) -> Result<(), String> {
    check_readable(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map(|m| m.permissions().mode())
            .map_err(|err| format!("could not stat ({err})"))?;
        if mode & 0o111 == 0 {
            return Err("not executable (missing +x permission)".to_string());
        }
    }
    Ok(())
}

/// Pre-flight check run before any case directory is created or process spawned: verifies
/// every file/binary the run needs can actually be opened (and, for the two binaries,
/// executed). Returns one formatted line per problem, empty if everything checks out — so
/// the GUI can show every broken path at once in a single popup, rather than the run failing
/// 6 times with the same opaque OS error and only the last failure's message surviving.
pub(crate) fn check_permissions(
    material_id_vtk: &Path,
    zone_id_vtk: Option<&Path>,
    env_script: &Path,
    amitex_path: &Path,
    mpirun_path: &Path,
    env_vars: &[(String, String)],
) -> Vec<String> {
    let mut problems = Vec::new();
    // Material XML (-m) and Algorithm XML (-a) aren't user-browsed files anymore — they're
    // generated from the Configuration tab's editors and written fresh into run_dir right
    // before the run starts, so there's nothing pre-existing to check permissions on here.
    let mut readable_inputs = vec![
        ("Material ID VTK (-nm)", material_id_vtk),
        ("env_amitex.sh (or equivalent)", env_script),
    ];
    if let Some(zone_id_vtk) = zone_id_vtk {
        readable_inputs.push(("Zone ID VTK (-nz)", zone_id_vtk));
    }
    for (label, path) in readable_inputs {
        if let Err(reason) = check_readable(path) {
            problems.push(format!("{label}: {} — {reason}", path.display()));
        }
    }
    for (label, path) in [("amitex_fftp binary", amitex_path), ("mpirun binary", mpirun_path)] {
        if let Err(reason) = check_executable(path) {
            problems.push(format!("{label}: {} — {reason}", path.display()));
        }
    }
    // Not one of the GUI-configured paths, but AMITEX resolves it implicitly at runtime for
    // native material behaviors (empty `Lib=""` in mat.xml) — and it's exactly the file that
    // turned out to be blocked (Gatekeeper quarantine/ACL) while the top-level binary itself
    // opened fine, so it needs its own check rather than being invisible until the run is
    // already 6-for-6 failed. Derived from the sourced `AMITEX_PATH`, not guessed from the
    // binary's own directory layout.
    if let Some((_, amitex_path_root)) = env_vars.iter().find(|(k, _)| k == "AMITEX_PATH") {
        let native_lib = Path::new(amitex_path_root).join("libAmitex/src/materiaux/libUmatAmitex.so");
        if native_lib.is_file() {
            if let Err(reason) = check_readable(&native_lib) {
                problems.push(format!(
                    "Native material behavior library: {} — {reason}",
                    native_lib.display()
                ));
            }
        }
    }
    problems
}

fn run_pipeline(config: PipelineConfig, tx: Sender<RunEvent>) {
    if let Err(err) = check_binary_exists(&config.mpirun_path, "mpirun binary")
        .and_then(|()| check_binary_exists(&config.amitex_path, "amitex_fftp binary"))
    {
        let _ = tx.send(RunEvent::Fatal(err.to_string()));
        return;
    }

    let mut cases = Vec::with_capacity(6);
    for index in 0..6 {
        match prepare_case(&config, index) {
            Ok(case) => cases.push(case),
            Err(err) => {
                let _ = tx.send(RunEvent::Fatal(err.to_string()));
                return;
            }
        }
    }

    // Cases run one after another, not concurrently: each `mpirun amitex_fftp` call
    // already claims all available ranks/cores on its own, so running 6 at once would
    // just make them contend for the same CPU rather than finish sooner.
    let mut all_ok = true;
    for case in &cases {
        let _ = tx.send(RunEvent::CaseStarted { case: case.label.clone() });
        let mut command = Command::new(&config.mpirun_path);
        // Whatever `env_amitex.sh` (or equivalent) set up — AMITEX_PATH, PATH,
        // LD_LIBRARY_PATH for FFTW/OpenMPI/MFront, etc. — applied verbatim, matching what
        // sourcing that script in a real shell before running `mpirun` would give it.
        command.envs(config.env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        command.arg(&config.amitex_path).arg("-nm").arg(&config.material_id_vtk);
        if let Some(zone_id_vtk) = &config.zone_id_vtk {
            command.arg("-nz").arg(zone_id_vtk);
        }
        let spawned = command
            .arg("-m")
            .arg(&config.mat_xml)
            .args(["-c", "char.xml"])
            .arg("-a")
            .arg(&config.algo_xml)
            .args(["-s", &format!("_{}", case.label)])
            // Escape hatch for AMITEX flags this GUI doesn't model explicitly (present or
            // future) — see `PipelineConfig::extra_args`.
            .args(&config.extra_args)
            .current_dir(&case.dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match spawned {
            Ok(child) => child,
            Err(err) => {
                let _ = tx.send(RunEvent::Fatal(format!(
                    "{}: failed to spawn {}: {err}",
                    case.label,
                    config.mpirun_path.display()
                )));
                let _ = tx.send(RunEvent::CaseFinished { case: case.label.clone(), success: false });
                all_ok = false;
                continue;
            }
        };

        // Best-effort: a failure to open this just means no on-disk trace, not a failed run.
        let log_file = std::fs::File::create(command_log_path(case))
            .ok()
            .map(|f| Arc::new(Mutex::new(f)));

        // stdout and stderr are drained on separate threads so a full pipe on one
        // can't stall the other (or the child) while we wait for it to exit.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let readers: Vec<_> = [
            stdout.map(|p| (Box::new(p) as Box<dyn std::io::Read + Send>, "stdout")),
            stderr.map(|p| (Box::new(p) as Box<dyn std::io::Read + Send>, "stderr")),
        ]
        .into_iter()
        .flatten()
        .map(|(pipe, stream_name)| {
            let tx = tx.clone();
            let label = case.label.clone();
            let log_file = log_file.clone();
            thread::spawn(move || stream_output(pipe, label, stream_name, log_file, tx))
        })
        .collect();

        let status = child.wait();
        for reader in readers {
            let _ = reader.join();
        }
        let success = matches!(status, Ok(s) if s.success());
        if success {
            postprocess_case(case, &tx);
        }
        let _ = tx.send(RunEvent::CaseFinished { case: case.label.clone(), success });
        all_ok &= success;
    }

    if all_ok {
        let std_paths: [PathBuf; 6] = std::array::from_fn(|i| std_path(&cases[i]));
        let _ = tx.send(RunEvent::AllFinished { std_paths });
    }
}

/// Kicks off the 6-case run on a background thread and returns a channel of progress
/// events for the GUI to poll each frame.
pub(crate) fn spawn_pipeline(config: PipelineConfig) -> Receiver<RunEvent> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || run_pipeline(config, tx));
    rx
}

#[allow(dead_code)]
pub(crate) fn case_dir(run_dir: &Path, index: usize) -> PathBuf {
    run_dir.join(case_label(index))
}
