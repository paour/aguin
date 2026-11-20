use std::path::PathBuf;

use eframe::egui::{self, Key, Modifiers};
use serde::{Deserialize, Serialize};

use crate::history_view::{HistorySettings, HistoryView};
use crate::threads_view::{ThreadsSettings, ThreadsView};

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
enum Tab {
    #[default]
    History,
    Threads,
}

/// Saved by eframe with the window position and egui's own state.
/// `serde(default)` lets a file written by an older version still load.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Settings {
    tab: Tab,
    history: HistorySettings,
    threads: ThreadsSettings,
}

pub struct Paths {
    pub history: PathBuf,
    pub threads: PathBuf,
    pub home: Option<String>,
    pub local_host: Option<String>,
}

pub struct App {
    tab: Tab,
    history: HistoryView,
    threads: ThreadsView,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Paths) -> Self {
        let settings: Settings = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();
        Self {
            tab: settings.tab,
            history: HistoryView::new(
                cc.egui_ctx.clone(),
                paths.history,
                paths.home.clone(),
                settings.history,
            ),
            threads: ThreadsView::new(
                cc.egui_ctx.clone(),
                paths.threads,
                paths.home,
                paths.local_host,
                settings.threads,
            ),
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.history.poll();
        self.threads.poll();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let (history, threads) = ui.ctx().input_mut(|input| {
            (
                input.consume_key(Modifiers::COMMAND, Key::Num1),
                input.consume_key(Modifiers::COMMAND, Key::Num2),
            )
        });
        if history {
            self.tab = Tab::History;
        }
        if threads {
            self.tab = Tab::Threads;
        }
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::History, "Shell history")
                    .on_hover_text("Cmd+1");
                ui.selectable_value(&mut self.tab, Tab::Threads, "Agent threads")
                    .on_hover_text("Cmd+2");
            });
            ui.add_space(2.0);
        });
        match self.tab {
            Tab::History => self.history.ui(ui),
            Tab::Threads => self.threads.ui(ui),
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let settings = Settings {
            tab: self.tab,
            history: self.history.settings(),
            threads: self.threads.settings(),
        };
        eframe::set_value(storage, eframe::APP_KEY, &settings);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_settings_fill_in_defaults() {
        let settings: Settings =
            serde_json::from_str(r#"{"tab": "Threads", "history": {"filter": {"query": "git"}}}"#)
                .unwrap();
        assert_eq!(settings.tab, Tab::Threads);
        assert_eq!(settings.history.filter.query, "git");
        assert_eq!(
            settings.history.filter.exit,
            crate::history_filter::ExitFilter::All
        );
        assert_eq!(settings.history.selected, None);
        assert!(!settings.threads.show_subagents);
    }

    #[test]
    fn settings_survive_a_round_trip() {
        let mut settings = Settings::default();
        settings.history.filter.host = Some("mac".into());
        settings.threads.selected = Some("aaaa-1111".into());
        let back: Settings =
            serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        assert_eq!(back.history.filter.host.as_deref(), Some("mac"));
        assert_eq!(back.threads.selected.as_deref(), Some("aaaa-1111"));
    }
}
