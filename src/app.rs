use crate::pipeline;
pub(crate) use crate::pipeline::SimulationModes;
use crate::pipeline::runner::{PipelineConfig, RunEvent};
use crate::postproc;
use crate::postproc::moduli::{self, ElasticModuli};
use crate::viewer3d;
use directories::{ProjectDirs, UserDirs};
use eframe::{egui, egui_glow, glow};
use rfd::FileDialog;
use serde::{Deserialize, Serialize};
use std::default::Default;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Arc;

#[derive(Default)]
pub struct AmitexGui {
    current_tab: Tab,
    config_tab: ConfigTab,
    run_tab: RunTab,
    materials_tab: MaterialsTab,
    /// The glow context, captured once at startup (only `Some` when running with the Glow
    /// renderer, which is eframe's default). Needed to build/rebuild GL resources for the
    /// materials viewer outside of a `PaintCallback`, e.g. when the user clicks "Load".
    gl_ctx: Option<Arc<glow::Context>>,
    /// Set once `start_run` launches a pipeline; polled every frame in `poll_run`.
    run_state: Option<RunState>,
    /// Pre-flight validation error from clicking "Run" before a `run_state` exists.
    run_error: Option<String>,
    /// Files/binaries clicking "Run" found unreadable or non-executable, shown as a blocking
    /// popup instead of launching a run that would just fail identically 6 times.
    permission_errors: Option<Vec<String>>,
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
    /// Where AMITEX wrote this run's case directories (`PipelineConfig::run_dir`) — kept
    /// here too (not just consumed by the pipeline) so "Save" knows what to copy.
    run_dir: PathBuf,
    /// Set once the user clicks "Save" and a destination is picked; `Ok` holds the folder
    /// actually written to.
    save_result: Option<Result<PathBuf, String>>,
}

/// The subset of `ConfigTab` that's persisted across launches via eframe's storage. Covers
/// every field a user would otherwise have to re-browse-to/re-type each launch; excludes
/// `simulation_mode` (only one option exists right now, nothing to restore).
#[derive(Default, Serialize, Deserialize)]
struct PersistedConfig {
    name: String,
    custom_name: bool,
    zone_id_vtk: String,
    material_id_vtk: String,
    algorithm: String,
    materials_xml: String,
    env_script: String,
    mpirun_path: String,
    extra_args: String,
    last_dir: Option<PathBuf>,
    /// Folder last picked in the "Save" dialog, so the next save starts there rather than
    /// always falling back to the Downloads folder.
    last_save_dir: Option<PathBuf>,
}

impl AmitexGui {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let persisted: PersistedConfig = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();

        Self {
            config_tab: ConfigTab {
                name: persisted.name,
                custom_name: persisted.custom_name,
                zone_id_vtk: persisted.zone_id_vtk,
                material_id_vtk: persisted.material_id_vtk,
                algorithm: persisted.algorithm,
                materials_xml: persisted.materials_xml,
                env_script: persisted.env_script,
                mpirun_path: persisted.mpirun_path,
                extra_args: persisted.extra_args,
                last_dir: persisted.last_dir,
                last_save_dir: persisted.last_save_dir,
                ..Default::default()
            },
            gl_ctx: cc.gl.clone(),
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
        let Some(env_script) = non_empty_path(&self.config_tab.env_script) else {
            self.run_error = Some("Select an env_amitex.sh (or equivalent) script first".to_string());
            return;
        };
        let Some(mpirun_path) = non_empty_path(&self.config_tab.mpirun_path) else {
            self.run_error = Some("Select the mpirun binary first".to_string());
            return;
        };

        // Sourced rather than browsed-to directly: picks up whatever PATH/LD_LIBRARY_PATH/
        // AMITEX_PATH the install's own env script sets up, not just a guessed AMITEX_PATH.
        let env_vars = match pipeline::runner::source_env_script(&env_script) {
            Ok(vars) => vars,
            Err(err) => {
                self.run_error = Some(err.to_string());
                return;
            }
        };
        let Some(amitex_path) = pipeline::runner::resolve_amitex_binary(&env_vars) else {
            self.run_error = Some(
                "Could not find amitex_fftp via the sourced script (checked AMITEX_PATH and \
                 PATH) — check it sets one of these correctly"
                    .to_string(),
            );
            return;
        };

        // Optional, unlike the paths above: AMITEX assumes one zone per material if
        // `-nz` is omitted, so an empty field isn't an error.
        let zone_id_vtk = non_empty_path(&self.config_tab.zone_id_vtk);
        // Simple whitespace split rather than full shell-quoting support: covers passing
        // AMITEX flags this GUI has no dedicated field for (present or future) without
        // needing a code change, at the cost of not supporting args containing spaces.
        let extra_args: Vec<String> =
            self.config_tab.extra_args.split_whitespace().map(str::to_string).collect();

        let problems = pipeline::runner::check_permissions(
            &material_id_vtk,
            zone_id_vtk.as_deref(),
            &mat_xml,
            &algo_xml,
            &env_script,
            &amitex_path,
            &mpirun_path,
            &env_vars,
        );
        if !problems.is_empty() {
            self.permission_errors = Some(problems);
            return;
        }

        let Some(dirs) = ProjectDirs::from("com", "AMITEX", "AMITEX-GUI") else {
            self.run_error =
                Some("Could not determine a data directory for run output".to_string());
            return;
        };
        let run_dir = dirs.data_dir().join("runs").join(&self.config_tab.name);

        let rx = pipeline::runner::spawn_pipeline(PipelineConfig {
            run_dir: run_dir.clone(),
            material_id_vtk,
            zone_id_vtk,
            mat_xml,
            algo_xml,
            amitex_path,
            mpirun_path,
            env_vars,
            extra_args,
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
            run_dir,
            save_result: None,
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

        let Some(state) = &mut self.run_state else {
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

        if ui.add_enabled(!state.log.is_empty(), egui::Button::new("Copy log")).clicked() {
            ui.ctx().copy_text(state.log.join("\n"));
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
                render_matrix(ui, "matrix_c", "Stiffness matrix C", &moduli.c);
                render_matrix(ui, "matrix_s", "Compliance matrix S", &moduli.s);
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, format!("Homogenization failed: {err}"));
            }
            None => {}
        }

        ui.separator();
        // Enabled once the run has stopped producing events, success or failure — a
        // failed run's logs/partial output are still worth saving for debugging.
        if ui.add_enabled(state.done, egui::Button::new("Save…")).clicked() {
            let start_dir = self
                .config_tab
                .last_save_dir
                .clone()
                .or_else(|| UserDirs::new().and_then(|dirs| dirs.download_dir().map(Path::to_path_buf)))
                .or_else(home_dir);

            let mut dialog = FileDialog::new();
            if let Some(dir) = start_dir {
                dialog = dialog.set_directory(dir);
            }
            if let Some(dest_parent) = dialog.pick_folder() {
                self.config_tab.last_save_dir = Some(dest_parent.clone());
                let input = postproc::save::SaveInput {
                    run_dir: &state.run_dir,
                    name: &self.config_tab.name,
                    simulation_mode: &self.config_tab.simulation_mode,
                    moduli: state.moduli.as_ref(),
                    log: &state.log,
                };
                state.save_result =
                    Some(postproc::save::save_run(&dest_parent, input).map_err(|err| err.to_string()));
            }
        }
        match &state.save_result {
            Some(Ok(path)) => {
                ui.colored_label(egui::Color32::GREEN, format!("Saved to {}", path.display()));
            }
            Some(Err(err)) => {
                ui.colored_label(egui::Color32::RED, format!("Save failed: {err}"));
            }
            None => {}
        }
    }

    /// Blocking popup listing every file/binary "Run" couldn't open (or, for the two
    /// binaries, execute) — set by `start_run`'s pre-flight `pipeline::runner::check_permissions`
    /// call. Dismissing it just clears the list; it doesn't retry the check, since the user
    /// needs to go fix the underlying permissions/paths first.
    fn render_permission_errors_modal(&mut self, ctx: &egui::Context) {
        if self.permission_errors.is_none() {
            return;
        }
        let mut dismissed = false;
        egui::Modal::new(egui::Id::new("permission_errors")).show(ctx, |ui| {
            ui.set_max_width(480.0);
            ui.heading("Can't start run");
            ui.label("The following files couldn't be read (or, for binaries, executed):");
            ui.add_space(8.0);
            for problem in self.permission_errors.as_ref().unwrap() {
                ui.colored_label(egui::Color32::RED, problem);
            }
            ui.add_space(8.0);
            ui.label(
                "Check the path is correct and that this app has permission to access it \
                 (on macOS, a downloaded/extracted binary can be blocked by Gatekeeper \
                 quarantine or a stray ACL even though the path exists).",
            );
            ui.add_space(8.0);
            if ui.add(egui::Button::new("OK")).clicked() {
                dismissed = true;
            }
        });
        if dismissed {
            self.permission_errors = None;
        }
    }
}

#[derive(Default, PartialEq)]
enum Tab {
    #[default]
    Config,
    Run,
}

/// Angular speed (rad/s) of the idle auto-spin — same direction and magnitude whether
/// it's spinning from app start or resuming after a drag, so "resume" never resets it.
const SPIN_VELOCITY: f32 = 0.35;
const DRAG_SENSITIVITY: f32 = 0.008;
/// Clamp on pitch (radians, ~74 degrees) so orbiting can't flip over the poles.
const MAX_PITCH: f32 = 1.3;
const IDLE_RESUME_SECS: f64 = 10.0;
const VIEWER_HEIGHT: f32 = 360.0;

struct MaterialsTab {
    renderer: Option<viewer3d::GlRenderer>,
    loaded_path: Option<PathBuf>,
    /// The `ConfigTab::material_id_vtk` text last seen, whether or not it loaded
    /// successfully — compared each frame against the live field so a newly browsed/typed
    /// path triggers a load automatically, without needing a manual "Load / Refresh" click.
    last_seen_path: String,
    legend: Vec<(f64, [f32; 3])>,
    bounding_radius: f32,
    error: Option<String>,
    yaw: f32,
    pitch: f32,
    dragging: bool,
    spin_active: bool,
    /// Set when a drag ends; once `IDLE_RESUME_SECS` have passed with no new drag, the
    /// auto-spin resumes from wherever `yaw` currently is (never reset to a start value).
    idle_since: Option<f64>,
}

impl Default for MaterialsTab {
    fn default() -> Self {
        Self {
            renderer: None,
            loaded_path: None,
            last_seen_path: String::new(),
            legend: Vec::new(),
            bounding_radius: 1.0,
            error: None,
            yaw: 0.0,
            pitch: 0.45,
            dragging: false,
            // Spinning from the moment the viewer first appears, per the original ask.
            spin_active: true,
            idle_since: None,
        }
    }
}

impl MaterialsTab {
    fn load(&mut self, gl_ctx: Option<&Arc<glow::Context>>, material_id_vtk: &str) {
        self.error = None;

        let Some(gl) = gl_ctx else {
            self.error = Some("No OpenGL context available".to_string());
            return;
        };
        let Some(path) = non_empty_path(material_id_vtk) else {
            self.error = Some("Select a Material ID VTK file first".to_string());
            return;
        };
        let grid = match crate::postproc::vtkio::read_vtk_cell_scalars(&path) {
            Ok(grid) => grid,
            Err(err) => {
                self.error = Some(err.to_string());
                return;
            }
        };
        let mesh = viewer3d::build_boundary_mesh(&grid);
        let renderer = match viewer3d::GlRenderer::new(gl, &mesh) {
            Ok(renderer) => renderer,
            Err(err) => {
                self.error = Some(err);
                return;
            }
        };

        if let Some(old) = self.renderer.take() {
            old.destroy(gl);
        }
        self.bounding_radius = mesh.bounding_radius;
        self.legend = mesh.legend;
        self.loaded_path = Some(path);
        self.renderer = Some(renderer);
    }

    fn ui(&mut self, ui: &mut egui::Ui, gl_ctx: Option<&Arc<glow::Context>>, material_id_vtk: &str) {
        if material_id_vtk != self.last_seen_path {
            self.last_seen_path = material_id_vtk.to_string();
            if !material_id_vtk.trim().is_empty() {
                self.load(gl_ctx, material_id_vtk);
            }
        }

        ui.horizontal(|ui| {
            if ui.button("Load / Refresh").clicked() {
                self.load(gl_ctx, material_id_vtk);
            }
            if let Some(path) = &self.loaded_path {
                ui.label(format!("Loaded: {}", path.display()));
            }
        });

        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::RED, err);
        }

        if !self.legend.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (id, color) in &self.legend {
                    let c = egui::Color32::from_rgb(
                        (color[0] * 255.0) as u8,
                        (color[1] * 255.0) as u8,
                        (color[2] * 255.0) as u8,
                    );
                    ui.colored_label(c, "⬛");
                    ui.label(format!("material {id:.0}"));
                }
            });
        }

        let Some(renderer) = &self.renderer else {
            ui.label("Load a Material ID VTK file to view it.");
            return;
        };

        // Fixed rather than `ui.available_height()`: inside the `ScrollArea` wrapping this
        // panel, available height is effectively unbounded, which would blow the viewport
        // up to an enormous size instead of a sane fixed one.
        let size = egui::vec2(ui.available_width(), VIEWER_HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::drag());

        let now = ui.input(|i| i.time);
        let dt = ui.input(|i| i.stable_dt);

        if response.dragged() {
            self.dragging = true;
            self.spin_active = false;
            self.idle_since = None;
            let delta = response.drag_delta();
            self.yaw -= delta.x * DRAG_SENSITIVITY;
            self.pitch = (self.pitch + delta.y * DRAG_SENSITIVITY).clamp(-MAX_PITCH, MAX_PITCH);
        }
        if response.drag_stopped() {
            self.dragging = false;
            self.idle_since = Some(now);
        }

        if !self.dragging {
            if let Some(since) = self.idle_since {
                if now - since >= IDLE_RESUME_SECS {
                    self.spin_active = true;
                    self.idle_since = None;
                }
            }
        }

        if self.spin_active {
            self.yaw = (self.yaw + SPIN_VELOCITY * dt).rem_euclid(std::f32::consts::TAU);
        }

        if self.dragging || self.spin_active {
            ui.ctx().request_repaint();
        } else if let Some(since) = self.idle_since {
            let remaining = (IDLE_RESUME_SECS - (now - since)).max(0.0);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(remaining));
        }

        let radius = self.bounding_radius.max(0.5);
        let distance = radius * 3.0;
        let eye = glam::Vec3::new(
            distance * self.pitch.cos() * self.yaw.sin(),
            distance * self.pitch.sin(),
            distance * self.pitch.cos() * self.yaw.cos(),
        );
        let view = glam::camera::rh::view::look_at_mat4(eye, glam::Vec3::ZERO, glam::Vec3::Y);
        let light_dir = eye.normalize_or_zero().to_array();
        // 15% headroom around the mesh's bounding sphere — fixed regardless of orientation,
        // so the cube's on-screen size never changes as it rotates (no pulsing/"zoom" look).
        let margin = 1.15;

        // The aspect ratio *must* come from the paint callback's actual pixel viewport, not
        // from `rect` here: egui_glow clamps the callback's viewport to the window's current
        // pixel bounds (see `ViewportInPixels::from_points`), so whenever `rect` extends past
        // the edge of the window (shrunk smaller, or scrolled partly out of view), the real
        // GL viewport ends up narrower/shorter than `rect` on one axis only. Building the
        // projection from `rect`'s aspect instead of the clamped one is exactly what was
        // stretching the render — non-uniformly — to fill that mismatched box.
        let renderer = renderer.clone();
        let callback = egui_glow::CallbackFn::new(move |info, painter| {
            let vp = info.viewport_in_pixels();
            let aspect = (vp.width_px.max(1) as f32 / vp.height_px.max(1) as f32).max(0.01);
            let (half_width, half_height) = if aspect >= 1.0 {
                (radius * aspect * margin, radius * margin)
            } else {
                (radius * margin, (radius / aspect) * margin)
            };

            let proj = glam::camera::rh::proj::opengl::orthographic(
                -half_width,
                half_width,
                -half_height,
                half_height,
                0.01,
                distance + radius * 4.0,
            );
            let mvp = (proj * view).to_cols_array();
            renderer.paint(painter.gl(), mvp, light_dir);
        });
        ui.painter_at(rect)
            .add(egui::PaintCallback { rect, callback: Arc::new(callback) });

        draw_axis_gizmo(&ui.painter_at(rect), rect, view);
    }
}

/// Draws a small ParaView-style XYZ orientation gizmo in the top-right corner of `rect`,
/// using only `view`'s rotation (translation-independent) so it tracks the camera without
/// needing its own 3D draw call — three lines from a shared center, depth-sorted so the
/// axis pointing most toward the camera draws its label on top.
fn draw_axis_gizmo(painter: &egui::Painter, rect: egui::Rect, view: glam::Mat4) {
    let radius = 26.0;
    let backdrop_radius = radius + 16.0;
    // Offset from each edge by the *backdrop's* radius (not the smaller axis radius) plus
    // padding, so the backdrop circle itself never pokes past `rect` and gets clipped.
    let pad = 6.0;
    let center = egui::pos2(
        rect.right() - backdrop_radius - pad,
        rect.top() + backdrop_radius + pad,
    );

    painter.circle_filled(center, backdrop_radius, egui::Color32::from_black_alpha(70));

    let axes = [
        (glam::Vec3::X, egui::Color32::from_rgb(220, 70, 70), "X"),
        (glam::Vec3::Y, egui::Color32::from_rgb(90, 190, 90), "Y"),
        (glam::Vec3::Z, egui::Color32::from_rgb(90, 140, 230), "Z"),
    ];

    let mut projected: Vec<(f32, egui::Pos2, egui::Color32, &str)> = axes
        .iter()
        .map(|(dir, color, label)| {
            let v = view.transform_vector3(*dir);
            let tip = egui::pos2(center.x + v.x * radius, center.y - v.y * radius);
            (v.z, tip, *color, *label)
        })
        .collect();
    // Draw back-to-front so the axis closest to the camera ends up on top.
    projected.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    for (_, tip, color, label) in projected {
        painter.line_segment([center, tip], egui::Stroke::new(2.0, color));
        painter.circle_filled(tip, 5.0, color);
        painter.text(
            tip,
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
    }
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
///
/// `auto_detect`, if given, adds an "Auto" button to the left of "Browse…" that fills the
/// field from a best-effort search (see e.g. `pipeline::runner::find_mpirun`) instead of
/// requiring the user to know the path — silently does nothing if the search comes up empty,
/// since "Browse…" is always still right there as the fallback.
fn path_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    last_dir: &mut Option<PathBuf>,
    auto_detect: Option<fn() -> Option<PathBuf>>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        if let Some(auto_detect) = auto_detect {
            if ui.add(egui::Button::new("Auto")).clicked() {
                if let Some(found) = auto_detect() {
                    *last_dir = found.parent().map(Path::to_path_buf);
                    *value = found.display().to_string();
                }
            }
        }
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
        let response =
            ui.add(egui::TextEdit::singleline(value).desired_width(ui.available_width()));
        // Trailing whitespace (most often from a copy-paste) is invisible in the field but
        // breaks path lookups downstream, so it's stripped once editing settles rather than
        // on every keystroke, which would fight the user typing a path with interior spaces.
        if response.lost_focus() {
            let trimmed_len = value.trim_end().len();
            value.truncate(trimmed_len);
        }
    });
}

/// Renders a 6x6 matrix (the stiffness/compliance results) as a labeled grid, matching
/// what the legacy `runs2moduli.py` printed to the terminal. `id` must be unique among
/// grids on the same panel — `egui::Grid` uses it as the widget ID, and both matrices
/// are shown side by side in the same results section.
fn render_matrix(ui: &mut egui::Ui, id: &str, title: &str, m: &[[f64; 6]; 6]) {
    ui.label(title);
    egui::Grid::new(id).striped(true).show(ui, |ui| {
        for row in m {
            for value in row {
                ui.monospace(format!("{value:>9.4}"));
            }
            ui.end_row();
        }
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
    /// AMITEX `-nz` zone-ID map (zones *within* a material) — optional; AMITEX assumes one
    /// zone per material if left blank.
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
    /// Path to the install's `env_amitex.sh`-equivalent environment-setup script. Sourced
    /// (not just read) at run time to resolve `amitex_fftp`'s location and the full
    /// PATH/LD_LIBRARY_PATH/AMITEX_PATH environment it needs — replaces browsing to the
    /// `amitex_fftp` binary directly, since that alone doesn't tell us anything about the
    /// shared libraries (FFTW/OpenMPI/MFront) it was built against.
    env_script: String,
    mpirun_path: String,
    /// Raw extra `amitex_fftp` CLI flags/values (whitespace-separated), appended verbatim
    /// after the standard ones — covers any AMITEX option this GUI has no dedicated field
    /// for, present or future, without needing a code change.
    extra_args: String,
    /// Directory the last "Browse…" file dialog was opened/picked in, so the next dialog
    /// (for any field) starts there rather than always falling back to the home directory.
    last_dir: Option<PathBuf>,
    /// Folder the last "Save" dialog picked, so the next save starts there rather than
    /// always falling back to the Downloads folder.
    last_save_dir: Option<PathBuf>,
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

        // Passed to amitex_fftp as -nm: maps which cells belong to which material/phase,
        // so mat.xml's per-numM properties land in the right places.
        path_field(
            ui,
            "Material ID VTK (-nm):",
            &mut self.material_id_vtk,
            &mut self.last_dir,
            None,
        );

        // Passed to amitex_fftp as -nz if set; optional, so left blank AMITEX assumes one
        // zone per material.
        path_field(
            ui,
            "Zone ID VTK (-nz):",
            &mut self.zone_id_vtk,
            &mut self.last_dir,
            None,
        );

        path_field(ui, "Algorithm:", &mut self.algorithm, &mut self.last_dir, None);
        path_field(ui, "Material:", &mut self.materials_xml, &mut self.last_dir, None);

        ui.separator();

        // Sourced (not just browsed-to) at run time — resolves both amitex_fftp's location
        // and the environment (PATH/LD_LIBRARY_PATH/AMITEX_PATH) it needs to actually run.
        path_field(
            ui,
            "env_amitex.sh:",
            &mut self.env_script,
            &mut self.last_dir,
            None,
        );
        path_field(
            ui,
            "mpirun:",
            &mut self.mpirun_path,
            &mut self.last_dir,
            Some(pipeline::runner::find_mpirun),
        );

        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Additional amitex_fftp arguments:");
            ui.add(
                egui::TextEdit::singleline(&mut self.extra_args)
                    .desired_width(ui.available_width())
                    .hint_text("e.g. -sm -sz  (appended verbatim, whitespace-separated)"),
            );
        });
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
                name: self.config_tab.name.clone(),
                custom_name: self.config_tab.custom_name,
                zone_id_vtk: self.config_tab.zone_id_vtk.clone(),
                material_id_vtk: self.config_tab.material_id_vtk.clone(),
                algorithm: self.config_tab.algorithm.clone(),
                materials_xml: self.config_tab.materials_xml.clone(),
                env_script: self.config_tab.env_script.clone(),
                mpirun_path: self.config_tab.mpirun_path.clone(),
                extra_args: self.config_tab.extra_args.clone(),
                last_dir: self.config_tab.last_dir.clone(),
                last_save_dir: self.config_tab.last_save_dir.clone(),
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

        self.render_permission_errors_modal(ui.ctx());

        egui::CentralPanel::default().show(ui, |ui| {
            // The three sections stacked below can exceed the window height (especially
            // with the materials viewer's fixed-height 3D view); without a scroll area
            // egui doesn't clip to a new viewport, it clips to whatever's left of the
            // *window*, silently cutting off whatever's currently below the fold.
            // `auto_shrink` defaults to true on both axes, which would let the scroll
            // area (and everything sized off its width, like the viewer's
            // `ui.available_width()`) shrink-to-fit rather than filling the panel —
            // that's what was "squishing" the 3D view narrower while scrolling.
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                section(ui, "Configuration", true, |ui| {
                    self.config_tab.ui(ui);
                });

                section(ui, "Run & Results", true, |ui| {
                    self.render_results(ui);
                });

                section(ui, "Materials Viewer", true, |ui| {
                    self.materials_tab.ui(
                        ui,
                        self.gl_ctx.as_ref(),
                        &self.config_tab.material_id_vtk,
                    );
                });
            });
        });
    }
}
