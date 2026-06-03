// On Windows, suppress the console window that would otherwise appear
// alongside the GUI window.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app;

use app::{GuiLayer, LogEntry, LogLevel};
use std::sync::{Arc, Mutex};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> eframe::Result<()> {
    // Minimal CLI args. Env var MC_SHARE_TELEMETRY=1 also works on the
    // platforms that support it, but Windows users find the flag form
    // easier (no shell prefix syntax to deal with).
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("MineScale-Java GUI\n");
        println!("USAGE:");
        println!("  mc-share-gui [--telemetry]\n");
        println!("OPTIONS:");
        println!("  --telemetry   Send anonymous connection diagnostics to the");
        println!("                coordination server. Off by default. See README");
        println!("                section \"接続調査への協力について\" for details.");
        println!("  -h, --help    Show this help and exit.");
        return Ok(());
    }
    let telemetry_flag = args.iter().any(|a| a == "--telemetry");

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
        Box::new(move |_cc| Ok(Box::new(app::App::new(rt, log, telemetry_flag)) as Box<dyn eframe::App>)),
    )
}
