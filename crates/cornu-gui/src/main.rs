mod app;
mod client_settings;
mod cookies;
mod editor;
mod environments;
mod events;
mod folder;
mod format;
mod highlight;
mod history;
mod import;
mod oauth;
mod project;
mod query_editor;
mod response;
mod runner;
mod schema;
mod tree;
mod websocket;
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
        Box::new(|cc| {
            let workspace = std::env::args_os()
                .nth(1)
                .map(std::path::PathBuf::from)
                .or_else(|| app::last_workspace(cc.storage));
            Ok(Box::new(app::CornuApp::new(
                worker::Worker::new()?,
                workspace,
            )))
        }),
    )
}
