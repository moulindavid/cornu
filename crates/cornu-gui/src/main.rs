mod app;
mod editor;
mod format;
mod widgets;
mod worker;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("cornu")
            .with_inner_size([1200.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "cornu",
        options,
        Box::new(|_cc| Ok(Box::new(app::CornuApp::new(worker::Worker::new()?)))),
    )
}
