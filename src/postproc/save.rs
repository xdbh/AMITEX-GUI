use crate::pipeline::SimulationModes;
use crate::postproc::moduli::ElasticModuli;
use anyhow::Context;
use std::fs;
use std::path::{Path, PathBuf};

/// Everything "Save" needs to snapshot a finished (or failed) run: the AMITEX-generated
/// case directories on disk, plus the results/context that only ever existed in memory.
pub(crate) struct SaveInput<'a> {
    pub(crate) run_dir: &'a Path,
    pub(crate) name: &'a str,
    pub(crate) simulation_mode: &'a SimulationModes,
    pub(crate) moduli: Option<&'a Result<ElasticModuli, String>>,
    pub(crate) log: &'a [String],
}

/// Copies `input.run_dir` (the 6 `load_<xx>/` case directories AMITEX wrote — `.std`
/// results, `.log`/`_command.log` logs, `sig*.vtk`) into a new timestamped folder under
/// `dest_parent`, alongside a `summary.txt` (simulation type + derived moduli/matrices)
/// and the GUI's own captured log. Returns the folder actually written to.
pub(crate) fn save_run(dest_parent: &Path, input: SaveInput) -> anyhow::Result<PathBuf> {
    let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let dest = dest_parent.join(format!("{}_{timestamp}", input.name));
    fs::create_dir_all(&dest).with_context(|| format!("creating {}", dest.display()))?;

    copy_dir_recursive(input.run_dir, &dest)
        .with_context(|| format!("copying {} to {}", input.run_dir.display(), dest.display()))?;

    fs::write(dest.join("gui_log.txt"), input.log.join("\n"))
        .with_context(|| format!("writing {}", dest.join("gui_log.txt").display()))?;

    fs::write(dest.join("summary.txt"), build_summary(&input))
        .with_context(|| format!("writing {}", dest.join("summary.txt").display()))?;

    Ok(dest)
}

fn build_summary(input: &SaveInput) -> String {
    let mut out = String::new();
    out.push_str(&format!("Simulation: {}\n", input.name));
    out.push_str(&format!("Type: {}\n", input.simulation_mode.label()));
    out.push_str(&format!("Saved: {}\n\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")));

    match input.moduli {
        Some(Ok(moduli)) => {
            out.push_str(&format_matrix("Stiffness matrix C", &moduli.c));
            out.push('\n');
            out.push_str(&format_matrix("Compliance matrix S", &moduli.s));
            out.push('\n');
            out.push_str(&format!("E       = {:.4}\n", moduli.e));
            out.push_str(&format!("nu      = {:.4}\n", moduli.nu));
            out.push_str(&format!("G       = {:.4}\n", moduli.g));
            out.push_str(&format!("Zener A = {:.4}\n", moduli.zener));
        }
        Some(Err(err)) => out.push_str(&format!("Homogenization failed: {err}\n")),
        None => out.push_str("Run did not complete.\n"),
    }
    out
}

fn format_matrix(title: &str, m: &[[f64; 6]; 6]) -> String {
    let mut out = format!("{title}:\n");
    for row in m {
        let cells: Vec<String> = row.iter().map(|v| format!("{v:>10.4}")).collect();
        out.push_str(&cells.join(" "));
        out.push('\n');
    }
    out
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}
