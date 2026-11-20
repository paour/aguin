use std::path::PathBuf;

use eframe::egui::{self, Key, Modifiers};
use egui_native_look as look;
use serde::{Deserialize, Serialize};

use crate::history_view::{HistorySettings, HistoryView};
use crate::menu::{Command, MenuBar};
use crate::theme;
use crate::threads_view::{ThreadsSettings, ThreadsView};

/// The height of the macOS title bar, so that the tabs line up with the traffic lights.
const TITLE_BAR_HEIGHT: f32 = 28.0;
const TAB_LABELS: [&str; 2] = ["Shell history", "Agent threads"];
const DOCS_URL: &str = "https://docs.atuin.sh/";

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, Serialize, Deserialize)]
enum Tab {
    #[default]
    History,
    Threads,
}

impl Tab {
    const ALL: [Self; 2] = [Self::History, Self::Threads];

    fn index(self) -> usize {
        Self::ALL.iter().position(|&tab| tab == self).unwrap_or(0)
    }
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
    menu: MenuBar,
    /// The state the menu's check mark shows, set again only when the view's changes.
    subagents_in_menu: Option<bool>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Paths) -> Self {
        theme::install(&cc.egui_ctx);
        let settings: Settings = cc
            .storage
            .and_then(|storage| eframe::get_value(storage, eframe::APP_KEY))
            .unwrap_or_default();
        Self {
            menu: MenuBar::install(&cc.egui_ctx),
            subagents_in_menu: None,
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

impl App {
    /// Runs a menu command. The menu takes the key equivalents it shows, so the views never
    /// see those keys on macOS and do the same thing here instead.
    fn run(&mut self, ctx: &egui::Context, command: Command) {
        match command {
            Command::ShowHistory => self.tab = Tab::History,
            Command::ShowThreads => self.tab = Tab::Threads,
            Command::ToggleSubagents => self.threads.toggle_subagents(),
            Command::Find => match self.tab {
                Tab::History => self.history.find(),
                Tab::Threads => self.threads.find(),
            },
            Command::SearchAll => match self.tab {
                Tab::History => self.history.find(),
                Tab::Threads => self.threads.search_all(),
            },
            Command::FindNext | Command::FindPrevious => {
                if self.tab == Tab::Threads {
                    self.threads.find_step(command == Command::FindNext);
                }
            }
            // egui-winit reads the clipboard for these and hands egui the matching event.
            Command::Cut => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut),
            Command::Copy => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy),
            Command::Paste => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
            Command::Undo => press(ctx, Key::Z, Modifiers::COMMAND),
            Command::Redo => press(ctx, Key::Z, Modifiers::COMMAND.plus(Modifiers::SHIFT)),
            Command::SelectAll => press(ctx, Key::A, Modifiers::COMMAND),
            Command::OpenDocs => ctx.open_url(egui::OpenUrl::new_tab(DOCS_URL)),
        }
        ctx.request_repaint();
    }

    /// The tabs, centred in the bar. On macOS the bar is the window's title bar: dragging it
    /// moves the window, a double click zooms it, and the traffic lights keep their room.
    fn title_bar(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        if cfg!(target_os = "macos") {
            let _ = look::TitleBar::new("").show(ui, rect);
        }
        let mut index = self.tab.index();
        let tabs = look::Segmented::new("tabs", &mut index, &TAB_LABELS)
            .tooltips(&["Cmd+1", "Cmd+2"])
            .width(260.0);
        let height = ui.spacing().interact_size.y;
        ui.put(
            egui::Rect::from_center_size(rect.center(), egui::vec2(260.0, height)),
            tabs,
        );
        self.tab = Tab::ALL[index.min(Tab::ALL.len() - 1)];
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        theme::follow_system(ctx);
        self.history.poll();
        self.threads.poll();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        for command in self.menu.take_commands() {
            self.run(ui.ctx(), command);
        }
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
        egui::Panel::top("tabs")
            .exact_size(TITLE_BAR_HEIGHT)
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill))
            .show(ui, |ui| self.title_bar(ui));
        match self.tab {
            Tab::History => self.history.ui(ui),
            Tab::Threads => self.threads.ui(ui),
        }
        let subagents = self.threads.subagents_shown();
        if self.subagents_in_menu != Some(subagents) {
            self.menu.set_subagents(subagents);
            self.subagents_in_menu = Some(subagents);
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

/// Gives egui the key press that the menu took, for the text field that has the focus.
fn press(ctx: &egui::Context, key: Key, modifiers: Modifiers) {
    ctx.input_mut(|input| {
        input.events.push(egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        });
    });
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
