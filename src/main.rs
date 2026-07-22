mod app;
mod pipeline;
mod postproc;
mod preproc;

use app::AmitexGui;

fn main() {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "AMITEX-GUI",
        native_options,
        Box::new(|cc| Ok(Box::new(AmitexGui::new(cc)))),
    );
}
