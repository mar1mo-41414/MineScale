mod app;

use app::{GuiLayer, LogEntry, LogLevel};
use std::sync::{Arc, Mutex};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> eframe::Result<()> {
    // Shared log buffer — filled by the tracing layer, read by the GUI.
    let log: Arc<Mutex<Vec<LogEntry>>> = Arc::new(Mutex::new(Vec::new()));

    // Wire tracing → GUI log.  All info!/warn!/error! calls in client code
    // will appear in the log panel automatically.
    tracing_subscriber::registry()
        .with(GuiLayer(Arc::clone(&log)))
        .init();

    let rt = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime"),
    );

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("MineScale-Java")
            .with_inner_size([660.0, 560.0])
            .with_min_inner_size([560.0, 460.0]),
        ..Default::default()
    };

    eframe::run_native(
        "MineScale-Java",
        native_options,
        Box::new(move |_cc| Ok(Box::new(app::App::new(rt, log)) as Box<dyn eframe::App>)),
    )
}
