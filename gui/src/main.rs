mod app;

use std::sync::Arc;

fn main() -> eframe::Result<()> {
    // Tokio runtime lives for the whole process lifetime.
    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime"),
    );

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MineScale-Java")
            .with_inner_size([640.0, 540.0])
            .with_min_inner_size([560.0, 460.0])
            .with_resizable(true),
        ..Default::default()
    };

    eframe::run_native(
        "MineScale-Java",
        native_options,
        Box::new(move |_cc| Ok(Box::new(app::App::new(rt)) as Box<dyn eframe::App>)),
    )
}
