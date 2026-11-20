mod app;
mod format;
mod history_db;
mod history_filter;
mod history_view;
mod menu;
mod sqlite;
mod theme;
mod threads_db;
mod threads_find;
mod threads_view;

use std::path::PathBuf;
use std::process::ExitCode;

use eframe::egui;

fn main() -> ExitCode {
    // Finder can start an app with a `-psn_…` process serial number argument.
    let arg = std::env::args_os()
        .nth(1)
        .filter(|arg| !arg.to_string_lossy().starts_with("-psn_"));
    if matches!(arg.as_ref().and_then(|a| a.to_str()), Some("-h" | "--help")) {
        println!(
            "Usage: aguin [HISTORY_DB]\n\n\
             Shows the atuin shell history, and the coding-agent threads that atuin captures, in a window.\n\
             Both databases are opened read-only.\n\
             Without HISTORY_DB, it uses db_path from the atuin config, else ~/.local/share/atuin/history.db.\n\
             Threads are read from threads.db in the atuin data directory."
        );
        return ExitCode::SUCCESS;
    }
    let Some(home) = std::env::home_dir() else {
        eprintln!("Cannot find the atuin data directory because HOME is not set.");
        return ExitCode::FAILURE;
    };
    let paths = app::Paths {
        history: arg.map_or_else(|| history_db::default_path(&home), PathBuf::from),
        threads: threads_db::default_path(&home),
        local_host: threads_db::local_host_id(&home),
        home: Some(home.to_string_lossy().into_owned()),
    };

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Aguin")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([720.0, 400.0])
        // macOS: the content runs under a transparent title bar, where the app puts its tabs.
        .with_fullsize_content_view(true)
        .with_titlebar_shown(false)
        .with_title_shown(false);
    if let Some(icon) = app_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    let result = eframe::run_native(
        "aguin",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, paths)))),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{err}");
            ExitCode::FAILURE
        }
    }
}

/// eframe sets the Dock icon at start, to the egui logo unless the app gives its own.
fn app_icon() -> Option<egui::IconData> {
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).ok()
}

#[cfg(test)]
mod tests {
    #[test]
    fn app_icon_decodes() {
        let icon = super::app_icon().unwrap();
        assert_eq!((icon.width, icon.height), (1024, 1024));
    }
}
