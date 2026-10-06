//! ytfast desktop command.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> eframe::Result<()> {
    let paths = ytfast::backend::Paths::new();
    let logging = fastframe_log::Logging::new("ytfast", env!("CARGO_PKG_VERSION"))
        .filter("warn,ytfast=info")
        .file(paths.data.join("ytfast.log"))
        .panic_log(paths.data.join("panics.log"))
        .init();
    if let Err(error) = logging {
        eprintln!("logging unavailable: {error}");
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("ytfast")
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([960.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "ytfast",
        options,
        Box::new(|cc| Ok(Box::new(ytfast::app::App::new(cc)))),
    )
}
