use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use eframe::egui::{
    self, Align, ComboBox, Key, Label, Layout, Modifiers, RichText, TextEdit, TextWrapMode,
};
use egui_extras::{Column, TableBuilder};
use serde::{Deserialize, Serialize};

use crate::format::{self, group_digits, tilde};
use crate::history_db::{self, Entry, Update};
use crate::history_filter::{ExitFilter, Filter};

/// What the history tab restores at the next start.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub filter: Filter,
    /// `Entry::id` of the selected command.
    pub selected: Option<String>,
}

/// How the next frame scrolls the table to the selected row.
#[derive(Clone, Copy)]
enum ScrollTo {
    Visible,
    Center,
}

pub struct HistoryView {
    db_path: PathBuf,
    home: Option<String>,
    updates: Receiver<Update>,
    loaded: bool,
    error: Option<String>,
    entries: Vec<Entry>,
    /// Hosts with their command counts, most used first.
    hosts: Vec<(String, usize)>,
    filter: Filter,
    /// The filter that produced `visible`. `None` forces a new pass.
    applied: Option<Filter>,
    /// Indices into `entries`, in display order.
    visible: Vec<usize>,
    /// Index into `entries`.
    selected: Option<usize>,
    scroll_to_selected: Option<ScrollTo>,
    /// The saved selection, applied once the history has loaded.
    restore_selection: Option<String>,
    focus_search: bool,
}

impl HistoryView {
    pub fn new(
        ctx: egui::Context,
        db_path: PathBuf,
        home: Option<String>,
        settings: HistorySettings,
    ) -> Self {
        let updates = history_db::spawn_loader(db_path.clone(), move || ctx.request_repaint());
        Self {
            db_path,
            home,
            updates,
            loaded: false,
            error: None,
            entries: Vec::new(),
            hosts: Vec::new(),
            filter: settings.filter,
            applied: None,
            visible: Vec::new(),
            selected: None,
            scroll_to_selected: None,
            restore_selection: settings.selected,
            focus_search: true,
        }
    }

    pub fn settings(&self) -> HistorySettings {
        HistorySettings {
            filter: self.filter.clone(),
            selected: self
                .selected
                .map(|index| self.entries[index].id.clone())
                .or_else(|| self.restore_selection.clone()),
        }
    }

    pub fn poll(&mut self) {
        while let Ok(update) = self.updates.try_recv() {
            match update {
                Update::Loaded(entries) => self.replace_entries(entries),
                Update::Failed(error) => self.error = Some(error),
            }
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.handle_keys(ui.ctx());
        egui::Panel::top("history_filters").show(ui, |ui| {
            ui.add_space(4.0);
            self.filter_bar(ui);
            ui.add_space(2.0);
        });
        if self.applied.as_ref() != Some(&self.filter) {
            self.visible = self.filter.apply(&self.entries);
            self.applied = Some(self.filter.clone());
        }
        egui::Panel::bottom("history_status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("history_details")
            .resizable(true)
            .default_size(380.0)
            .min_size(240.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                self.details(ui);
            });
        egui::CentralPanel::default().show(ui, |ui| {
            if !self.loaded {
                ui.centered_and_justified(|ui| ui.weak("Loading history…"));
            } else if self.visible.is_empty() {
                ui.centered_and_justified(|ui| ui.weak("No commands match the filters."));
            } else {
                self.table(ui);
            }
        });
    }

    fn replace_entries(&mut self, entries: Vec<Entry>) {
        let restoring = self.selected.is_none() && self.restore_selection.is_some();
        let selected_id = self
            .selected
            .map(|index| self.entries[index].id.clone())
            .or_else(|| self.restore_selection.take());
        self.selected = selected_id.and_then(|id| entries.iter().position(|e| e.id == id));
        if restoring {
            self.scroll_to_selected = Some(ScrollTo::Center);
        }
        self.hosts = count_hosts(&entries);
        self.entries = entries;
        self.applied = None;
        self.loaded = true;
        self.error = None;
    }

    fn selected_row(&self) -> Option<usize> {
        let selected = self.selected?;
        self.visible.iter().position(|&index| index == selected)
    }

    /// Runs before the widgets so that the search field does not also handle the arrow keys.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        let (find, down, up) = ctx.input_mut(|input| {
            (
                input.consume_key(Modifiers::COMMAND, Key::F),
                input.consume_key(Modifiers::NONE, Key::ArrowDown),
                input.consume_key(Modifiers::NONE, Key::ArrowUp),
            )
        });
        self.focus_search |= find;
        let Some(last) = self.visible.len().checked_sub(1) else {
            return;
        };
        let row = match (self.selected_row(), down, up) {
            (None, true, _) | (None, _, true) => 0,
            (Some(row), true, _) => (row + 1).min(last),
            (Some(row), _, true) => row.saturating_sub(1),
            _ => return,
        };
        self.selected = Some(self.visible[row]);
        self.scroll_to_selected = Some(ScrollTo::Visible);
    }

    fn filter_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let search = ui.add(
                TextEdit::singleline(&mut self.filter.query)
                    .hint_text("Search commands")
                    .desired_width(320.0),
            );
            if std::mem::take(&mut self.focus_search) {
                search.request_focus();
            }
            ui.add(
                TextEdit::singleline(&mut self.filter.cwd)
                    .hint_text("Directory")
                    .desired_width(220.0),
            );
            ComboBox::from_id_salt("host")
                .selected_text(self.filter.host.as_deref().unwrap_or("All hosts"))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter.host, None, "All hosts");
                    for (host, count) in &self.hosts {
                        ui.selectable_value(
                            &mut self.filter.host,
                            Some(host.clone()),
                            format!("{host}  ({})", group_digits(*count)),
                        );
                    }
                });
            ComboBox::from_id_salt("exit")
                .selected_text(self.filter.exit.label())
                .show_ui(ui, |ui| {
                    for exit in ExitFilter::ALL {
                        ui.selectable_value(&mut self.filter.exit, exit, exit.label());
                    }
                });
            let session_chip = self
                .filter
                .session
                .as_deref()
                .map(|s| format!("Session {} ×", format::short_id(s)));
            if let Some(chip) = session_chip
                && ui
                    .button(chip)
                    .on_hover_text("Stop filtering by session")
                    .clicked()
            {
                self.filter.session = None;
            }
            if self.filter != Filter::default() && ui.button("Clear filters").clicked() {
                self.filter = Filter::default();
            }
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if !self.loaded {
                ui.spinner();
            }
            ui.weak(tilde(&self.db_path.to_string_lossy(), self.home.as_deref()).into_owned());
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(format!(
                    "{} of {} commands",
                    group_digits(self.visible.len()),
                    group_digits(self.entries.len())
                ));
            });
        });
    }

    fn table(&mut self, ui: &mut egui::Ui) {
        // Selectable labels would take the clicks that select a row.
        ui.style_mut().interaction.selectable_labels = false;
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::initial(130.0).at_least(80.0))
            .column(Column::initial(72.0).at_least(40.0))
            .column(Column::initial(40.0).at_least(30.0))
            // Not resizable: egui_extras freezes resizable widths, and this column must follow the window size.
            .column(
                Column::remainder()
                    .at_least(200.0)
                    .clip(true)
                    .resizable(false),
            )
            .column(Column::initial(240.0).at_least(60.0).clip(true))
            .column(Column::initial(110.0).at_least(50.0).clip(true));
        if let Some(scroll) = self.scroll_to_selected.take()
            && let Some(row) = self.selected_row()
        {
            let align = match scroll {
                ScrollTo::Visible => None,
                ScrollTo::Center => Some(Align::Center),
            };
            builder = builder.scroll_to_row(row, align);
        }

        let mut clicked = None;
        builder
            .header(row_height, |mut header| {
                for title in ["Time", "Duration", "Exit", "Command", "Directory", "Host"] {
                    header.col(|ui| {
                        ui.strong(title);
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, self.visible.len(), |mut row| {
                    let index = self.visible[row.index()];
                    let entry = &self.entries[index];
                    row.set_selected(self.selected == Some(index));
                    row.col(|ui| {
                        ui.label(format::time_from_nanos(entry.timestamp, "%Y-%m-%d %H:%M"));
                    });
                    row.col(|ui| {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.label(format::duration(entry.duration));
                        });
                    });
                    row.col(|ui| exit_label(ui, entry.exit));
                    row.col(|ui| {
                        ui.add(
                            Label::new(
                                RichText::new(format::first_line(&entry.command)).monospace(),
                            )
                            .truncate(),
                        );
                    });
                    row.col(|ui| {
                        ui.add(Label::new(tilde(&entry.cwd, self.home.as_deref())).truncate());
                    });
                    row.col(|ui| {
                        ui.add(Label::new(entry.host()).truncate());
                    });
                    if row.response().clicked() {
                        clicked = Some(index);
                    }
                });
            });
        if clicked.is_some() {
            self.selected = clicked;
        }
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let Some(entry) = self.selected.map(|index| &self.entries[index]) else {
            ui.weak("Select a command to see its details.");
            return;
        };
        let mut next_filter = None;

        ui.horizontal(|ui| {
            ui.heading("Command");
            if ui.button("Copy").clicked() {
                ui.ctx().copy_text(entry.command.clone());
            }
        });
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() * 0.5)
            .show(ui, |ui| {
                let mut command = entry.command.as_str();
                ui.add(
                    TextEdit::multiline(&mut command)
                        .code_editor()
                        .desired_rows(1)
                        .desired_width(f32::INFINITY),
                );
            });
        ui.add_space(8.0);

        egui::Grid::new("history_details_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.weak("Time");
                ui.label(format::time_from_nanos(
                    entry.timestamp,
                    "%a %Y-%m-%d %H:%M:%S %:z",
                ));
                ui.end_row();
                ui.weak("Duration");
                ui.label(format::non_empty_or(
                    format::duration(entry.duration),
                    "not recorded",
                ));
                ui.end_row();
                ui.weak("Exit code");
                if entry.exit == -1 {
                    ui.label("not recorded");
                } else {
                    exit_label(ui, entry.exit);
                }
                ui.end_row();
                ui.weak("Directory");
                ui.add(Label::new(entry.cwd.as_str()).wrap_mode(TextWrapMode::Wrap));
                ui.end_row();
                ui.weak("Host");
                ui.label(entry.host());
                ui.end_row();
                if let Some(user) = entry.user() {
                    ui.weak("User");
                    ui.label(user);
                    ui.end_row();
                }
                ui.weak("Session");
                ui.label(RichText::new(&entry.session).monospace());
                ui.end_row();
            });
        ui.add_space(8.0);

        ui.horizontal_wrapped(|ui| {
            if ui
                .button("Show session")
                .on_hover_text("Clear the filters and list only the commands of this session")
                .clicked()
            {
                next_filter = Some(Filter {
                    session: Some(entry.session.clone()),
                    ..Default::default()
                });
            }
            if ui.button("Filter by directory").clicked() {
                next_filter = Some(Filter {
                    cwd: entry.cwd.clone(),
                    ..self.filter.clone()
                });
            }
            if ui.button("Filter by host").clicked() {
                next_filter = Some(Filter {
                    host: Some(entry.host().to_owned()),
                    ..self.filter.clone()
                });
            }
        });

        if let Some(filter) = next_filter {
            self.filter = filter;
            self.scroll_to_selected = Some(ScrollTo::Center);
        }
    }
}

fn count_hosts(entries: &[Entry]) -> Vec<(String, usize)> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for entry in entries {
        *counts.entry(entry.host()).or_default() += 1;
    }
    let mut hosts: Vec<_> = counts
        .into_iter()
        .map(|(host, n)| (host.to_owned(), n))
        .collect();
    hosts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    hosts
}

fn exit_label(ui: &mut egui::Ui, exit: i64) {
    match exit {
        -1 => {}
        0 => {
            ui.weak("0");
        }
        code => {
            ui.colored_label(ui.visuals().error_fg_color, code.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_hosts_by_host_part() {
        let entry = |hostname: &str| Entry {
            hostname: hostname.into(),
            ..Default::default()
        };
        let entries = [entry("mac:me"), entry("mac:root"), entry("box")];
        assert_eq!(
            count_hosts(&entries),
            [("mac".to_owned(), 2), ("box".to_owned(), 1)]
        );
    }
}
