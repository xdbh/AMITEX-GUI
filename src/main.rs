mod app;
mod pipeline;
mod postproc;
mod preproc;
mod viewer3d;

use app::AmitexGui;

fn main() {
    let native_options = eframe::NativeOptions {
        // The materials viewer draws a rotatable 3D voxel mesh via a glow PaintCallback
        // (`egui_glow::CallbackFn`), which only the Glow renderer dispatches — the default
        // Wgpu renderer would silently skip it, expecting an `egui_wgpu::Callback` instead.
        renderer: eframe::Renderer::Glow,
        // Needed for correct depth-sorting of the 3D voxel mesh; egui's own 2D UI doesn't
        // use one (default is 0).
        depth_buffer: 24,
        ..Default::default()
    };
    eframe::run_native(
        "AMITEX-GUI",
        native_options,
        Box::new(|cc| Ok(Box::new(AmitexGui::new(cc)))),
    );
}
