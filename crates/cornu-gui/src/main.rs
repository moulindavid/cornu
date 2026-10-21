use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("cornu")
            .with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native("cornu", options, Box::new(|_cc| Ok(Box::new(CornuApp))))
}

struct CornuApp;

impl eframe::App for CornuApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(format!("cornu {}", cornu_core::VERSION));
        });
    }
}
