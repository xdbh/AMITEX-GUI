use crate::pipeline;
pub(crate) use crate::pipeline::SimulationModes;
use crate::pipeline::runner::{PipelineConfig, RunEvent};
use crate::postproc::moduli::{self, ElasticModuli};
use directories::{ProjectDirs, UserDirs};
use eframe::egui;
use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::default::Default;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Default)]
pub struct AmitexGui {
    current_tab: Tab,
    config_tab: ConfigTab,
    run_tab: RunTab,
    /// Set once `start_run` launches a pipeline; polled every frame in `poll_run`.
    run_state: Option<RunState>,
    /// Pre-flight validation error from clicking "Run" before a `run_state` exists.
    run_error: Option<String>,
}

#[derive(Clone, Copy, PartialEq)]
enum CaseStatus {
    Pending,
    Running,
    Succeeded,
    Failed,
}

impl CaseStatus {
    fn label(self) -> &'static str {
        match self {
            CaseStatus::Pending => "pending",
            CaseStatus::Running => "running",
            CaseStatus::Succeeded => "done",
            CaseStatus::Failed => "failed",
        }
    }
}

/// Updates `case`'s status in place, preserving `cases`' existing (Voigt) order.
fn set_case_status(cases: &mut [(String, CaseStatus)], case: &str, status: CaseStatus) {
    if let Some(entry) = cases.iter_mut().find(|(label, _)| label == case) {
        entry.1 = status;
    }
}

struct RunState {
    rx: Receiver<RunEvent>,
    /// Voigt order (xx, yy, zz, xy, xz, yz), matching `pipeline::loads::case_label` — a
    /// `Vec` rather than a sorted map so display order doesn't drift to alphabetical.
    cases: Vec<(String, CaseStatus)>,
    log: Vec<String>,
    moduli: Option<Result<ElasticModuli, String>>,
    fatal: Option<String>,
    done: bool,
}

/// The subset of `ConfigTab` that's persisted across launches via eframe's storage.
#[derive(Default, Serialize, Deserialize)]
struct PersistedConfig {
    amitex_path: String,
    mpirun_path: String,
}

impl AmitexGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let persisted: PersistedConfig = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();

        Self {
            config_tab: ConfigTab {
                amitex_path: persisted.amitex_path,
                mpirun_path: persisted.mpirun_path,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Validates the config is complete, then launches the 6-case homogenization run on a
    /// background thread. There are 6 cases because computing a 6x6 effective stiffness
    /// tensor needs one FFT solve per canonical unit-strain direction (Voigt order).
    fn start_run(&mut self) {
        self.run_error = None;

        let Some(material_id_vtk) = non_empty_path(&self.config_tab.material_id_vtk) else {
            self.run_error = Some("Select a Material ID VTK file first".to_string());
            return;
        };
        let Some(mat_xml) = non_empty_path(&self.config_tab.materials_xml) else {
            self.run_error = Some("Select a Material XML file first".to_string());
            return;
        };
        let Some(algo_xml) = non_empty_path(&self.config_tab.algorithm) else {
            self.run_error = Some("Select an Algorithm XML file first".to_string());
            return;
        };
        let Some(amitex_path) = non_empty_path(&self.config_tab.amitex_path) else {
            self.run_error = Some("Select the amitex_fftp binary first".to_string());
            return;
        };
        let Some(mpirun_path) = non_empty_path(&self.config_tab.mpirun_path) else {
            self.run_error = Some("Select the mpirun binary first".to_string());
            return;
        };

        let Some(dirs) = ProjectDirs::from("com", "AMITEX", "AMITEX-GUI") else {
            self.run_error =
                Some("Could not determine a data directory for run output".to_string());
            return;
        };
        let run_dir = dirs.data_dir().join("runs").join(&self.config_tab.name);

        let rx = pipeline::runner::spawn_pipeline(PipelineConfig {
            run_dir,
            material_id_vtk,
            mat_xml,
            algo_xml,
            amitex_path,
            mpirun_path,
        });

        let cases = (0..6)
            .map(|i| (pipeline::loads::case_label(i), CaseStatus::Pending))
            .collect();

        self.run_state = Some(RunState {
            rx,
            cases,
            log: Vec::new(),
            moduli: None,
            fatal: None,
            done: false,
        });
    }

    /// Drains any pending events from the background run thread. Called once per frame.
    fn poll_run(&mut self) {
        let Some(state) = &mut self.run_state else {
            return;
        };

        let mut std_paths = None;
        loop {
            match state.rx.try_recv() {
                Ok(RunEvent::CaseStarted { case }) => {
                    set_case_status(&mut state.cases, &case, CaseStatus::Running);
                }
                Ok(RunEvent::CaseOutput { case, line }) => {
                    state.log.push(format!("[{case}] {line}"));
                }
                Ok(RunEvent::CaseFinished { case, success }) => {
                    let status = if success {
                        CaseStatus::Succeeded
                    } else {
                        CaseStatus::Failed
                    };
                    set_case_status(&mut state.cases, &case, status);
                }
                Ok(RunEvent::AllFinished { std_paths: paths }) => std_paths = Some(paths),
                Ok(RunEvent::Fatal(msg)) => state.fatal = Some(msg),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    state.done = true;
                    break;
                }
            }
        }

        if let Some(paths) = std_paths {
            state.moduli = Some(moduli::compute_moduli(&paths).map_err(|err| err.to_string()));
            state.done = true;
        }
    }

    fn render_results(&mut self, ui: &mut egui::Ui) {
        if let Some(err) = &self.run_error {
            ui.colored_label(egui::Color32::RED, err);
        }

        let Some(state) = &self.run_state else {
            ui.label("No run yet.");
            return;
        };

        ui.horizontal(|ui| {
            for (label, status) in &state.cases {
                ui.label(format!("{label}: {}", status.label()));
            }
        });

        if let Some(fatal) = &state.fatal {
            ui.colored_label(egui::Color32::RED, fatal);
        }

        egui::ScrollArea::vertical()
            .max_height(150.0)
            .show(ui, |ui| {
                for line in &state.log {
                    ui.label(line);
                }
            });

        match &state.moduli {
            Some(Ok(moduli)) => {
                ui.separator();
                ui.label(format!("E = {:.4}", moduli.e));
                ui.label(format!("nu = {:.4}", moduli.nu));
                ui.label(format!("G = {:.4}", moduli.g));
                ui.label(format!("Zener A = {:.4}", moduli.zener));
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, format!("Homogenization failed: {err}"));
            }
            None => {}
        }
    }
}

#[derive(Default, PartialEq)]
enum Tab {
    #[default]
    Config,
    Run,
}

/// Returns `path` as a `PathBuf` unless it's empty (or all whitespace), which is how
/// an unset `ConfigTab` path field is represented.
fn non_empty_path(path: &str) -> Option<PathBuf> {
    (!path.trim().is_empty()).then(|| PathBuf::from(path))
}

fn home_dir() -> Option<PathBuf> {
    UserDirs::new().map(|dirs| dirs.home_dir().to_path_buf())
}

/// A labeled path field: editable text (so a path can be typed or pasted directly)
/// plus a "Browse…" button that opens a native file dialog and fills the text in.
/// The dialog starts in the field's own directory if set, else the last directory any
/// field's dialog was opened/picked in, else the user's home directory. `last_dir` is
/// updated on pick so the next field's dialog picks up from here.
fn path_field(ui: &mut egui::Ui, label: &str, value: &mut String, last_dir: &mut Option<PathBuf>) {
    ui.horizontal(|ui| {
        ui.label(label);
        if ui.add(egui::Button::new("Browse…")).clicked() {
            let start_dir = non_empty_path(value)
                .and_then(|p| p.parent().map(Path::to_path_buf))
                .or_else(|| last_dir.clone())
                .or_else(home_dir);

            let mut dialog = FileDialog::new();
            if let Some(dir) = start_dir {
                dialog = dialog.set_directory(dir);
            }
            if let Some(picked) = dialog.pick_file() {
                *last_dir = picked.parent().map(Path::to_path_buf);
                *value = picked.display().to_string();
            }
        }
        ui.add(egui::TextEdit::singleline(value).desired_width(320.0));
    });
}

/// A full-width expandable section with a divider above and below the header,
/// styled like a Photoshop/Blender-style collapsible panel. Built from the
/// stock `CollapsingHeader` widget inside a full-width `Frame` rather than
/// custom layout code, to avoid hit-test/highlight-rect mismatches.
fn section(
    ui: &mut egui::Ui,
    title: &str,
    default_open: bool,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    ui.separator();
    egui::Frame::default()
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(title)
                .default_open(default_open)
                .show(ui, add_contents);
        });
    ui.separator();
}

#[derive(Default)]
struct ConfigTab {
    name: String,
    custom_name: bool,
    /// AMITEX `-nz` zone-ID map (zones *within* a material) — not currently wired into
    /// the run command.
    zone_id_vtk: String,
    /// AMITEX `-nm` material-ID map — which cells belong to which material/phase.
    material_id_vtk: String,
    simulation_mode: pipeline::SimulationModes,
    //algorithm
    algorithm: String,
    // algorithm_type: AlgorithmType,
    // convergence_criterion: f64,
    // convergence_acceleration: bool,
    //mechanics
    // filter_type: FilterType,
    // small_perturbations: bool,
    materials_xml: String,
    amitex_path: String,
    mpirun_path: String,
    /// Directory the last "Browse…" file dialog was opened/picked in, so the next dialog
    /// (for any field) starts there rather than always falling back to the home directory.
    last_dir: Option<PathBuf>,
}

impl ConfigTab {
    fn ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Run Name:");

            if !self.custom_name {
                self.name = chrono::Local::now().format("%Y%m%d").to_string();
            }

            ui.add_enabled(self.custom_name, egui::TextEdit::singleline(&mut self.name));
            ui.checkbox(&mut self.custom_name, "Custom?");
        });

        // Passed to amitex_fftp as -nm: maps which cells belong to which material/phase,
        // so mat.xml's per-numM properties land in the right places.
        path_field(
            ui,
            "Material ID VTK (-nm):",
            &mut self.material_id_vtk,
            &mut self.last_dir,
        );

        // Not currently wired into the run command (no -nz support yet).
        path_field(
            ui,
            "Zone ID VTK (-nz):",
            &mut self.zone_id_vtk,
            &mut self.last_dir,
        );

        ui.horizontal(|ui| {
            ui.label("Simulation Mode:");
            egui::ComboBox::from_id_salt("filter_type")
                .selected_text(self.simulation_mode.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut self.simulation_mode,
                        SimulationModes::ElasticHomogenization,
                        SimulationModes::ElasticHomogenization.label(),
                    );
                });
        });

        path_field(ui, "Algorithm:", &mut self.algorithm, &mut self.last_dir);
        path_field(ui, "Material:", &mut self.materials_xml, &mut self.last_dir);

        ui.separator();

        path_field(
            ui,
            "amitex_fftp:",
            &mut self.amitex_path,
            &mut self.last_dir,
        );
        path_field(ui, "mpirun:", &mut self.mpirun_path, &mut self.last_dir);
    }
}
#[derive(Default)]
struct RunTab {
    custom_name: bool,
}

impl RunTab {
    fn ui(&mut self, ui: &mut egui::Ui) {}
}

impl eframe::App for AmitexGui {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(
            storage,
            eframe::APP_KEY,
            &PersistedConfig {
                amitex_path: self.config_tab.amitex_path.clone(),
                mpirun_path: self.config_tab.mpirun_path.clone(),
            },
        );
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.poll_run();
        if self.run_state.as_ref().is_some_and(|state| !state.done) {
            ui.ctx().request_repaint();
        }

        ui.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "AMITEX-GUI - {} - {}",
            self.config_tab.simulation_mode.label(),
            self.config_tab.name,
        )));
        egui::Panel::bottom("disclaimer").show(ui, |ui| {
            ui.horizontal(|ui| ui.label("DO NOT DISTRIBUTE, FOR INTERNAL USE ONLY PER AMITEX"));
        });

        egui::Panel::bottom("run_button").show(ui, |ui| {
            if ui.add(egui::Button::new("Run")).clicked() {
                self.start_run();
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            section(ui, "Configuration", true, |ui| {
                self.config_tab.ui(ui);
            });

            section(ui, "Run & Results", true, |ui| {
                self.render_results(ui);
            });
        });
    }
}
