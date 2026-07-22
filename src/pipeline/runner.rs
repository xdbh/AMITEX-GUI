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
    pub(crate) mat_xml: PathBuf,
    pub(crate) algo_xml: PathBuf,
    pub(crate) amitex_path: PathBuf,
    pub(crate) mpirun_path: PathBuf,
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

/// Derives `AMITEX_PATH` from the `amitex_fftp` binary path, per AMITEX's documented layout
/// `$AMITEX_PATH/libAmitex/bin/amitex_fftp`. Needed because native material behaviors (an
/// empty `Lib=""` in `mat.xml`) resolve their `.so` as `$AMITEX_PATH/libAmitex/src/materiaux/
/// libUmatAmitex.so` — with `AMITEX_PATH` unset, that `dlopen` fails and every rank aborts.
/// Returns `None` if the binary isn't laid out as expected, rather than guessing wrong.
fn amitex_path_env(amitex_path: &Path) -> Option<PathBuf> {
    let bin_dir = amitex_path.parent()?;
    let lib_amitex_dir = bin_dir.parent()?;
    if bin_dir.file_name()? != "bin" || lib_amitex_dir.file_name()? != "libAmitex" {
        return None;
    }
    Some(lib_amitex_dir.parent()?.to_path_buf())
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

fn run_pipeline(config: PipelineConfig, tx: Sender<RunEvent>) {
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

    let amitex_path_env = amitex_path_env(&config.amitex_path);
    if amitex_path_env.is_none() {
        let _ = tx.send(RunEvent::CaseOutput {
            case: "setup".to_string(),
            line: "Note: amitex_fftp isn't laid out as <root>/libAmitex/bin/amitex_fftp, so \
                   AMITEX_PATH wasn't set — native material behaviors (empty Lib=\"\" in \
                   mat.xml) may fail to load."
                .to_string(),
        });
    }

    // Cases run one after another, not concurrently: each `mpirun amitex_fftp` call
    // already claims all available ranks/cores on its own, so running 6 at once would
    // just make them contend for the same CPU rather than finish sooner.
    let mut all_ok = true;
    for case in &cases {
        let _ = tx.send(RunEvent::CaseStarted { case: case.label.clone() });
        let mut command = Command::new(&config.mpirun_path);
        if let Some(amitex_path_env) = &amitex_path_env {
            command.env("AMITEX_PATH", amitex_path_env);
        }
        let spawned = command
            .arg(&config.amitex_path)
            .arg("-nm")
            .arg(&config.material_id_vtk)
            .arg("-m")
            .arg(&config.mat_xml)
            .args(["-c", "char.xml"])
            .arg("-a")
            .arg(&config.algo_xml)
            .args(["-s", &format!("_{}", case.label)])
            .current_dir(&case.dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match spawned {
            Ok(child) => child,
            Err(err) => {
                let _ = tx.send(RunEvent::Fatal(format!("{}: {err}", case.label)));
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
