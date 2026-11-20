use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use eframe::egui::{
    self, Align, CollapsingHeader, Color32, FontId, Key, Label, Layout, Modifiers, RichText,
    TextFormat, TextStyle, WidgetText,
    collapsing_header::CollapsingState,
    text::{CCursor, LayoutJob},
};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use egui_extras::{Column, TableBuilder};
use egui_native_look as look;
use serde::{Deserialize, Serialize};

use crate::format::{self, compact, group_digits, tilde};
use crate::theme;
use crate::threads_db::{
    self, Block, Message, Request, Role, Thread, ToolCall, ToolOutput, Update,
};
use crate::threads_find::{ACTIVE_BG, Find, MATCH_BG, Marks, Part, markdown_id};

/// Shorter queries match most of the database by prefix, so they only filter titles.
const MIN_SEARCH_CHARS: usize = 3;
/// A block's message seq and position, which keeps its folded state across reloads.
type BlockId = (i64, usize);
/// Space kept for a message that has not been laid out yet.
const ESTIMATED_MESSAGE_HEIGHT: f32 = 60.0;
/// Messages this far outside the viewport are laid out too, so that scrolling does not show gaps.
const LAYOUT_MARGIN: f32 = 400.0;
/// Frames during which the view keeps scrolling to a match, while the heights above it settle.
const SCROLL_FRAMES: u8 = 4;

/// What the threads tab restores at the next start.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadsSettings {
    pub query: String,
    pub show_subagents: bool,
    /// `Thread::session_id` of the open thread, which stays the same when atuin rebuilds its tables.
    pub selected: Option<String>,
}

struct Transcript {
    thread: i64,
    messages: Vec<Message>,
    /// Heights measured when each message was last laid out, so that messages off screen are skipped.
    heights: Vec<Option<f32>>,
}

/// The search that the find bar asks for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Find the query again and keep the current match, as when the transcript grew.
    Refresh,
    /// Find the query again and move to the first match, as after a new query.
    Jump,
}

/// Find-in-thread: the query and its matches, and what the next frame must do with them.
#[derive(Default)]
struct FindBar {
    find: Find,
    pending: Option<Pending>,
    /// Next (`true`) or previous match.
    step: Option<bool>,
    scroll_frames: u8,
    focus: bool,
    /// Whether the find field had the keyboard focus last frame, so that the arrow keys stay its own.
    focused: bool,
}

impl FindBar {
    fn set_query(&mut self, query: &str) {
        query.clone_into(&mut self.find.query);
        self.pending = Some(Pending::Jump);
    }

    fn refresh(&mut self) {
        self.pending.get_or_insert(Pending::Refresh);
    }

    /// Finds the matches and moves between them as asked. Returns whether anything changed.
    fn update(&mut self, messages: &[Message], markdown: &mut CommonMarkCache) -> bool {
        let pending = self.pending.take();
        if let Some(pending) = pending {
            let current = self.find.current_index();
            self.find.search(messages, markdown);
            if pending == Pending::Jump {
                self.step = Some(true);
            } else {
                self.find.restore(current);
                self.find.activate(messages, markdown, false);
            }
        }
        let moved = self
            .step
            .take()
            .is_some_and(|forward| self.find.step(forward));
        if moved {
            self.find.activate(messages, markdown, true);
            self.scroll_frames = SCROLL_FRAMES;
        }
        pending.is_some() || moved
    }
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent UI flags, not a state machine"
)]
pub struct ThreadsView {
    db_path: PathBuf,
    home: Option<String>,
    local_host: Option<String>,
    requests: Sender<Request>,
    updates: Receiver<Update>,
    loaded: bool,
    error: Option<String>,
    threads: Vec<Thread>,
    query: String,
    show_subagents: bool,
    /// Threads with a message that matches the query, from atuin's full-text index.
    hits: Option<HashSet<i64>>,
    /// Set when `visible` must be computed again.
    dirty: bool,
    /// Indices into `threads`, in display order.
    visible: Vec<usize>,
    /// `Thread::id` of the open thread.
    selected: Option<i64>,
    transcript: Option<Transcript>,
    markdown: CommonMarkCache,
    find_bar: FindBar,
    scroll_list: bool,
    /// The saved thread, opened once the threads have loaded.
    restore_selection: Option<String>,
    focus_search: bool,
}

impl ThreadsView {
    pub fn new(
        ctx: egui::Context,
        db_path: PathBuf,
        home: Option<String>,
        local_host: Option<String>,
        settings: ThreadsSettings,
    ) -> Self {
        let (requests, updates) =
            threads_db::spawn_loader(db_path.clone(), move || ctx.request_repaint());
        let mut view = Self {
            db_path,
            home,
            local_host,
            requests,
            updates,
            loaded: false,
            error: None,
            threads: Vec::new(),
            query: settings.query,
            show_subagents: settings.show_subagents,
            hits: None,
            dirty: true,
            visible: Vec::new(),
            selected: None,
            transcript: None,
            markdown: CommonMarkCache::default(),
            find_bar: FindBar::default(),
            scroll_list: false,
            restore_selection: settings.selected,
            focus_search: true,
        };
        view.search_messages();
        view
    }

    pub fn settings(&self) -> ThreadsSettings {
        let selected = self
            .selected
            .and_then(|id| self.threads.iter().find(|t| t.id == id))
            .map(|thread| thread.session_id.clone());
        ThreadsSettings {
            query: self.query.clone(),
            show_subagents: self.show_subagents,
            selected: selected.or_else(|| self.restore_selection.clone()),
        }
    }

    /// Asks the loader for the threads whose messages match the query, when it is long enough.
    fn search_messages(&mut self) {
        if self.query.trim().chars().count() >= MIN_SEARCH_CHARS {
            let _ = self.requests.send(Request::Search(self.query.clone()));
        } else {
            self.hits = None;
        }
    }

    pub fn poll(&mut self) {
        while let Ok(update) = self.updates.try_recv() {
            match update {
                Update::Threads(threads) => {
                    self.threads = threads;
                    self.loaded = true;
                    self.error = None;
                    self.dirty = true;
                    let restored = self.restore_selection.take().and_then(|session_id| {
                        self.threads.iter().find(|t| t.session_id == session_id)
                    });
                    if let Some(id) = restored.map(|thread| thread.id) {
                        self.open(id);
                        self.scroll_list = true;
                        // A restored thread opens where it was left, not at its first match.
                        self.find_bar.pending = Some(Pending::Refresh);
                    }
                }
                Update::Transcript { thread, messages } if self.selected == Some(thread) => {
                    let mut heights = match self.transcript.take() {
                        Some(old) if old.thread == thread => old.heights,
                        _ => Vec::new(),
                    };
                    heights.resize(messages.len(), None);
                    self.transcript = Some(Transcript {
                        thread,
                        messages,
                        heights,
                    });
                    self.find_bar.refresh();
                }
                Update::Transcript { .. } => {}
                Update::SearchHits { query, threads } => {
                    if query == self.query {
                        self.hits = Some(threads);
                        self.dirty = true;
                    }
                }
                Update::Failed(error) => self.error = Some(error),
            }
        }
    }

    /// Cmd+F: the find field of the open thread, else the thread search.
    pub fn find(&mut self) {
        if self.selected.is_some() {
            self.find_bar.focus = true;
        } else {
            self.focus_search = true;
        }
    }

    /// Cmd+Shift+F: the search over every thread.
    pub fn search_all(&mut self) {
        self.focus_search = true;
    }

    pub fn find_step(&mut self, forward: bool) {
        self.find_bar.step = Some(forward);
    }

    pub fn toggle_subagents(&mut self) {
        self.show_subagents = !self.show_subagents;
        self.dirty = true;
    }

    pub fn subagents_shown(&self) -> bool {
        self.show_subagents
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.handle_keys(ui.ctx());
        egui::Panel::bottom("threads_status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("threads_list")
            .resizable(true)
            .default_size(420.0)
            .min_size(260.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                self.list_header(ui);
                ui.add_space(4.0);
                self.list(ui);
            });
        egui::CentralPanel::default().show(ui, |ui| self.transcript_ui(ui));
    }

    /// Opens a thread, and finds the thread list's query in it when there is one.
    fn open(&mut self, id: i64) {
        if self.selected != Some(id) {
            self.selected = Some(id);
            self.transcript = None;
            let _ = self.requests.send(Request::Open(id));
            if self.query.trim().is_empty() {
                self.find_bar.pending = Some(Pending::Jump);
            } else {
                self.find_bar.set_query(&self.query);
            }
        }
    }

    fn selected_row(&self) -> Option<usize> {
        let selected = self.selected?;
        self.visible
            .iter()
            .position(|&index| self.threads[index].id == selected)
    }

    /// Runs before the widgets so that the search fields do not also handle these keys.
    /// egui matches Cmd+F to Cmd+Shift+F too, so the Shift variants are taken first.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        let command_shift = Modifiers::COMMAND.plus(Modifiers::SHIFT);
        let find_field_focused = self.find_bar.focused;
        let (find_all, find, previous, next, down, up) = ctx.input_mut(|input| {
            (
                input.consume_key(command_shift, Key::F),
                input.consume_key(Modifiers::COMMAND, Key::F),
                input.consume_key(command_shift, Key::G),
                input.consume_key(Modifiers::COMMAND, Key::G),
                !find_field_focused && input.consume_key(Modifiers::NONE, Key::ArrowDown),
                !find_field_focused && input.consume_key(Modifiers::NONE, Key::ArrowUp),
            )
        });
        if find_all {
            self.search_all();
        } else if find {
            self.find();
        }
        if previous || next {
            self.find_step(next);
        }
        let Some(last) = self.visible.len().checked_sub(1) else {
            return;
        };
        let row = match (self.selected_row(), down, up) {
            (None, true, _) | (None, _, true) => 0,
            (Some(row), true, _) => (row + 1).min(last),
            (Some(row), _, true) => row.saturating_sub(1),
            _ => return,
        };
        self.open(self.threads[self.visible[row]].id);
        self.scroll_list = true;
    }

    fn list_header(&mut self, ui: &mut egui::Ui) {
        let search = ui.add(
            look::TextField::new(&mut self.query)
                .search()
                .hint("Search threads and messages"),
        );
        if std::mem::take(&mut self.focus_search) {
            search.request_focus();
        }
        if search.changed() {
            self.dirty = true;
            self.search_messages();
            self.find_bar.set_query(&self.query);
        }
        ui.horizontal(|ui| {
            if ui
                .add(look::Checkbox::new(&mut self.show_subagents, "Subagents"))
                .on_hover_text("Also list the threads that agents started for subtasks")
                .changed()
            {
                self.dirty = true;
            }
            if std::mem::take(&mut self.dirty) {
                self.visible = filter_threads(
                    &self.threads,
                    &self.query,
                    self.show_subagents,
                    self.hits.as_ref(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.weak(format!(
                    "{} of {} threads",
                    group_digits(self.visible.len()),
                    group_digits(self.threads.len())
                ));
            });
        });
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        // Selectable labels would take the clicks that select a row.
        ui.style_mut().interaction.selectable_labels = false;
        let row_height =
            ui.text_style_height(&TextStyle::Body) + ui.text_style_height(&TextStyle::Small) + 10.0;
        let mut table = TableBuilder::new(ui)
            .id_salt("threads_table")
            .sense(egui::Sense::click())
            .cell_layout(Layout::top_down(Align::Min))
            .column(Column::remainder().clip(true));
        if std::mem::take(&mut self.scroll_list)
            && let Some(row) = self.selected_row()
        {
            table = table.scroll_to_row(row, None);
        }
        let mut clicked = None;
        table.body(|body| {
            body.rows(row_height, self.visible.len(), |mut row| {
                let thread = &self.threads[self.visible[row.index()]];
                row.set_selected(self.selected == Some(thread.id));
                row.col(|ui| {
                    ui.add_space(3.0);
                    ui.add(
                        Label::new(theme::semibold(format::first_line(thread.display_title())))
                            .truncate(),
                    );
                    ui.add(
                        Label::new(RichText::new(self.subtitle(thread)).small().weak()).truncate(),
                    );
                });
                if row.response().clicked() {
                    clicked = Some(thread.id);
                }
            });
        });
        if let Some(id) = clicked {
            self.open(id);
        }
    }

    fn subtitle(&self, thread: &Thread) -> String {
        let mut parts = vec![
            format::time_from_millis(thread.updated_at, "%Y-%m-%d %H:%M"),
            thread.harness_name().into_owned(),
        ];
        if thread.is_subagent() {
            parts.push("subagent".to_owned());
        }
        if let Some(cwd) = &thread.cwd {
            parts.push(tilde(cwd, self.home.as_deref()).into_owned());
        }
        parts.push(format!("{} messages", group_digits(thread.message_count)));
        parts.join(" · ")
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if !self.loaded && self.error.is_none() {
                ui.spinner();
            }
            ui.weak(tilde(&self.db_path.to_string_lossy(), self.home.as_deref()).into_owned());
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }

    fn transcript_ui(&mut self, ui: &mut egui::Ui) {
        let Some(id) = self.selected else {
            let hint = if self.loaded {
                "Select a thread to read it."
            } else {
                "Loading threads…"
            };
            ui.centered_and_justified(|ui| ui.weak(hint));
            return;
        };
        let Some(thread) = self.threads.iter().find(|t| t.id == id) else {
            ui.centered_and_justified(|ui| ui.weak("This thread is no longer in the database."));
            return;
        };
        let mut open = None;
        thread_header(
            ui,
            &self.threads,
            thread,
            self.home.as_deref(),
            self.local_host.as_deref(),
            &mut open,
        );
        ui.add_space(4.0);
        find_bar_ui(ui, &mut self.find_bar);
        ui.separator();
        match &mut self.transcript {
            Some(transcript) if transcript.thread == id => {
                if self
                    .find_bar
                    .update(&transcript.messages, &mut self.markdown)
                {
                    ui.ctx().request_repaint();
                }
                let scroll = self.find_bar.scroll_frames > 0;
                let agent = thread.harness_name();
                let mut draw = Draw {
                    markdown: &mut self.markdown,
                    find: &self.find_bar.find,
                    agent: &agent,
                    scroll,
                };
                messages_ui(ui, transcript, &mut draw);
                if scroll {
                    self.find_bar.scroll_frames -= 1;
                    ui.ctx().request_repaint();
                }
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Loading messages…");
                });
            }
        }
        if let Some(id) = open {
            self.open(id);
        }
    }
}

/// Keeps subagents out unless asked, then matches every query word in the thread's text, or a message hit.
fn filter_threads(
    threads: &[Thread],
    query: &str,
    show_subagents: bool,
    hits: Option<&HashSet<i64>>,
) -> Vec<usize> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    threads
        .iter()
        .enumerate()
        .filter(|(_, thread)| {
            (show_subagents || !thread.is_subagent())
                && (terms
                    .iter()
                    .all(|term| thread.search_text.contains(term.as_str()))
                    || hits.is_some_and(|hits| hits.contains(&thread.id)))
        })
        .map(|(index, _)| index)
        .collect()
}

fn thread_header(
    ui: &mut egui::Ui,
    threads: &[Thread],
    thread: &Thread,
    home: Option<&str>,
    local_host: Option<&str>,
    open: &mut Option<i64>,
) {
    ui.add_space(6.0);
    ui.add(Label::new(RichText::new(thread.display_title()).heading()).wrap());
    ui.add_space(4.0);
    egui::Grid::new(("thread_header", thread.id))
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            let mut field = |name: &str, value: String| {
                ui.weak(name);
                ui.add(Label::new(value).wrap());
                ui.end_row();
            };
            let agent = match &thread.model {
                Some(model) => format!("{} · {model}", thread.harness_name()),
                None => thread.harness_name().into_owned(),
            };
            field("Agent", agent);
            if let Some(cwd) = &thread.cwd {
                field("Directory", tilde(cwd, home).into_owned());
            }
            if let Some(branch) = &thread.git_branch {
                field("Branch", branch.clone());
            }
            let time = "%a %Y-%m-%d %H:%M";
            field(
                "Time",
                format!(
                    "{} to {}",
                    format::time_from_millis(thread.started_at, time),
                    format::time_from_millis(thread.updated_at, time)
                ),
            );
            let tokens = thread.tokens;
            field(
                "Tokens",
                format!(
                    "{} in · {} out · {} cache read · {} cache write",
                    compact(tokens.input),
                    compact(tokens.output),
                    compact(tokens.cache_read),
                    compact(tokens.cache_write)
                ),
            );
            field("Messages", group_digits(thread.message_count));
            if let Some(host) = &thread.host_id {
                let host = if Some(host.as_str()) == local_host {
                    "this machine".to_owned()
                } else {
                    format!("another machine ({})", format::short_id(host))
                };
                field("Host", host);
            }
            field("Session", thread.session_id.clone());
        });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        let command = thread.resume_command();
        if ui
            .add(look::Button::new("Copy resume command"))
            .on_hover_text(&command)
            .clicked()
        {
            ui.ctx().copy_text(command);
        }
        if ui.add(look::Button::new("Copy session id")).clicked() {
            ui.ctx().copy_text(thread.session_id.clone());
        }
        let parent = thread
            .parent_session_id
            .as_deref()
            .and_then(|parent| threads.iter().find(|t| t.session_id == parent));
        if let Some(parent) = parent
            && ui.add(look::Button::new("Open parent thread")).clicked()
        {
            *open = Some(parent.id);
        }
    });
    let children: Vec<&Thread> = threads
        .iter()
        .filter(|t| t.parent_session_id.as_deref() == Some(thread.session_id.as_str()))
        .collect();
    if !children.is_empty() {
        CollapsingHeader::new(format!("Subagents and forks ({})", children.len()))
            .id_salt(("children", thread.id))
            .show(ui, |ui| {
                for child in children {
                    if ui.link(format::first_line(child.display_title())).clicked() {
                        *open = Some(child.id);
                    }
                }
            });
    }
}

fn find_bar_ui(ui: &mut egui::Ui, bar: &mut FindBar) {
    ui.horizontal(|ui| {
        let field = ui.add(
            look::TextField::new(&mut bar.find.query)
                .search()
                .id_salt("find_in_thread")
                .hint("Find in thread")
                .width(260.0),
        );
        if std::mem::take(&mut bar.focus) {
            field.request_focus();
        }
        if field.changed() {
            bar.pending = Some(Pending::Jump);
        }
        // A single-line field gives up the focus on Enter, so it is taken back to keep typing.
        if field.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter)) {
            bar.step = Some(!ui.input(|input| input.modifiers.shift));
            field.request_focus();
        }
        bar.focused = field.has_focus();
        if ui
            .add(look::Button::new("Previous"))
            .on_hover_text("Shift+Enter or Cmd+Shift+G")
            .clicked()
        {
            bar.step = Some(false);
        }
        if ui
            .add(look::Button::new("Next"))
            .on_hover_text("Enter or Cmd+G")
            .clicked()
        {
            bar.step = Some(true);
        }
        ui.weak(bar.find.counter());
    });
}

/// Where a block is: its message's index in the transcript and seq, and its index in the message.
#[derive(Clone, Copy)]
struct At {
    message: usize,
    seq: i64,
    block: usize,
}

impl At {
    fn id(self) -> BlockId {
        (self.seq, self.block)
    }
}

/// What drawing the transcript needs besides the messages.
struct Draw<'a> {
    markdown: &'a mut CommonMarkCache,
    find: &'a Find,
    agent: &'a str,
    /// Set while the view moves to the current match.
    scroll: bool,
}

impl<'a> Draw<'a> {
    fn marks(&self, at: At, part: Part) -> Option<Marks<'a>> {
        self.find.marks(at.message, at.block, part)
    }

    fn scrolls_to(&self, at: At, part: Part) -> bool {
        self.scroll
            && self.find.current().is_some_and(|hit| {
                (hit.message, hit.block, hit.part) == (at.message, at.block, part)
            })
    }

    /// Unfolds a block while the view moves to a match in its body.
    fn opens(&self, at: At) -> bool {
        self.scroll
            && self.find.current().is_some_and(|hit| {
                (hit.message, hit.block) == (at.message, at.block) && hit.part != Part::Title
            })
    }
}

fn messages_ui(ui: &mut egui::Ui, transcript: &mut Transcript, draw: &mut Draw) {
    // The message being moved to is laid out even off screen, so that its match can be scrolled to.
    let target = draw
        .find
        .current()
        .filter(|_| draw.scroll)
        .map(|hit| hit.message);
    egui::ScrollArea::vertical()
        .id_salt(("transcript", transcript.thread))
        .auto_shrink(false)
        .show_viewport(ui, |ui, viewport| {
            let origin = ui.cursor().top();
            let wanted = viewport.expand2(egui::vec2(0.0, LAYOUT_MARGIN));
            for (index, message) in transcript.messages.iter().enumerate() {
                let top = ui.cursor().top() - origin;
                let height = transcript.heights[index];
                let above = height.is_some_and(|height| top + height < wanted.top());
                if (above || top > wanted.bottom()) && target != Some(index) {
                    ui.add_space(height.unwrap_or(ESTIMATED_MESSAGE_HEIGHT));
                    continue;
                }
                // Consecutive messages from one speaker read as one turn, under one header.
                let header = index == 0 || transcript.messages[index - 1].role != message.role;
                message_ui(ui, index, message, draw, header);
                transcript.heights[index] = Some(ui.cursor().top() - origin - top);
            }
        });
}

fn message_ui(ui: &mut egui::Ui, index: usize, message: &Message, draw: &mut Draw, header: bool) {
    if header {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let name = match &message.role {
                Role::User => RichText::new("You").color(ui.visuals().hyperlink_color),
                Role::Assistant => RichText::new(draw.agent),
                Role::Tool => RichText::new("Tool"),
                Role::System => RichText::new("System"),
                Role::Other(name) => RichText::new(name),
            };
            ui.label(name.family(look::bold_family()));
            ui.weak(format::time_from_millis(
                message.timestamp,
                "%Y-%m-%d %H:%M:%S",
            ));
        });
    }
    for (block_index, block) in message.blocks.iter().enumerate() {
        let at = At {
            message: index,
            seq: message.seq,
            block: block_index,
        };
        block_ui(ui, draw, at, block, message.role == Role::User);
    }
}

fn block_ui(ui: &mut egui::Ui, draw: &mut Draw, at: At, block: &Block, from_user: bool) {
    let body = TextStyle::Body.resolve(ui.style());
    match block {
        Block::Text(text) if from_user => {
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .corner_radius(6)
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let color = ui.visuals().text_color();
                    let marks = draw.marks(at, Part::Text);
                    let scroll = draw.scrolls_to(at, Part::Text);
                    marked_label(ui, text, &body, color, marks.as_ref(), scroll);
                });
        }
        Block::Text(text) => markdown_ui(ui, draw, at, text),
        Block::Reasoning(tokens) => {
            let text = match tokens {
                Some(tokens) => format!("Thinking · {} tokens", group_digits(tokens)),
                None => "Thinking".to_owned(),
            };
            ui.label(RichText::new(text).small().italics().weak());
        }
        Block::Tool(call) => tool_ui(ui, draw, at, call),
        Block::Output(output) => {
            let title = if output.files.is_empty() {
                "Tool output"
            } else {
                "File changes"
            };
            let title = failure_color(ui, RichText::new(title), output.error);
            foldable(ui, at.id(), title, draw.opens(at), false, |ui| {
                output_ui(ui, draw, at, output);
            });
        }
        Block::Summary(text) => {
            let title = format!("Summary: {}", format::first_line(text));
            let open = draw.opens(at);
            foldable(
                ui,
                at.id(),
                RichText::new(title).italics(),
                open,
                false,
                |ui| {
                    markdown_ui(ui, draw, at, text);
                },
            );
        }
        Block::Error(text) => {
            let color = ui.visuals().error_fg_color;
            let marks = draw.marks(at, Part::Text);
            let scroll = draw.scrolls_to(at, Part::Text);
            marked_label(ui, text, &body, color, marks.as_ref(), scroll);
        }
        Block::Other(json) => {
            let open = draw.opens(at);
            foldable(
                ui,
                at.id(),
                RichText::new("Unknown block"),
                open,
                false,
                |ui| {
                    let marks = draw.marks(at, Part::Text);
                    code(ui, json, marks.as_ref(), draw.scrolls_to(at, Part::Text));
                },
            );
        }
    }
}

/// `egui_commonmark` numbers tables from 0 under the parent's id, so without a scope of its own
/// every message's first table would share one set of column widths.
fn markdown_ui(ui: &mut egui::Ui, draw: &mut Draw, at: At, text: &str) {
    let id = markdown_id(at.seq, at.block);
    if draw.scrolls_to(at, Part::Text) {
        // egui_commonmark scrolls to its active match once per request, and the target may still move.
        draw.markdown
            .search_cache_mut(&id)
            .scroll_to_active_search_match();
    }
    ui.push_id(at.id(), |ui| {
        CommonMarkViewer::new()
            .search_match_color(MATCH_BG)
            .search_active_match_color(ACTIVE_BG)
            .show_with_id(id, ui, draw.markdown, text);
    });
}

fn tool_ui(ui: &mut egui::Ui, draw: &mut Draw, at: At, call: &ToolCall) {
    let failed = call.output.as_ref().is_some_and(|output| output.error);
    let color = if failed {
        ui.visuals().error_fg_color
    } else {
        ui.visuals().text_color()
    };
    let title: WidgetText = match draw.marks(at, Part::Title) {
        Some(marks) => {
            let format = TextFormat::simple(TextStyle::Monospace.resolve(ui.style()), color);
            let mut job = LayoutJob::default();
            append_marked(&mut job, &call.title, 0, &marks, &mut 0, &format);
            job.into()
        }
        None => RichText::new(&call.title).monospace().color(color).into(),
    };
    let open = draw.opens(at);
    let scroll_title = draw.scrolls_to(at, Part::Title);
    foldable(ui, at.id(), title, open, scroll_title, |ui| {
        if !call.input.is_empty() {
            let marks = draw.marks(at, Part::Input);
            code(
                ui,
                &call.input,
                marks.as_ref(),
                draw.scrolls_to(at, Part::Input),
            );
        }
        match &call.output {
            Some(output) => output_ui(ui, draw, at, output),
            None => {
                ui.weak("No output was captured.");
            }
        }
    });
}

/// Cuts the title to the panel width: a wider title would widen the transcript and stop its text from wrapping.
fn foldable(
    ui: &mut egui::Ui,
    id: BlockId,
    title: impl Into<WidgetText>,
    open: bool,
    scroll_title: bool,
    add_body: impl FnOnce(&mut egui::Ui),
) {
    let id = ui.make_persistent_id(id);
    let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, false);
    if open {
        state.set_open(true);
    }
    let mut clicked = false;
    let mut header = state.show_header(ui, |ui| {
        let title = ui.add(Label::new(title).truncate().sense(egui::Sense::click()));
        if scroll_title {
            title.scroll_to_me(Some(Align::Center));
        }
        clicked = title.clicked();
    });
    if clicked {
        header.toggle();
    }
    header.body(add_body);
}

fn failure_color(ui: &egui::Ui, text: RichText, failed: bool) -> RichText {
    if failed {
        text.color(ui.visuals().error_fg_color)
    } else {
        text
    }
}

fn output_ui(ui: &mut egui::Ui, draw: &Draw, at: At, output: &ToolOutput) {
    if !output.text.is_empty() {
        ui.weak(if output.error { "Failed:" } else { "Output:" });
        let marks = draw.marks(at, Part::Output);
        code(
            ui,
            &output.text,
            marks.as_ref(),
            draw.scrolls_to(at, Part::Output),
        );
    }
    for (index, file) in output.files.iter().enumerate() {
        ui.label(theme::semibold(format!("{} {}", file.change, file.path)));
        if !file.diff.is_empty() {
            let part = Part::Diff(index);
            let marks = draw.marks(at, part);
            diff(ui, &file.diff, marks.as_ref(), draw.scrolls_to(at, part));
        }
    }
}

fn code_frame(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(4)
        .inner_margin(6)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui);
        });
}

fn code(ui: &mut egui::Ui, text: &str, marks: Option<&Marks>, scroll: bool) {
    code_frame(ui, |ui| {
        let font = TextStyle::Monospace.resolve(ui.style());
        let color = ui.visuals().text_color();
        marked_label(ui, text, &font, color, marks, scroll);
    });
}

fn diff(ui: &mut egui::Ui, text: &str, marks: Option<&Marks>, scroll: bool) {
    let font = TextStyle::Monospace.resolve(ui.style());
    let added = if ui.visuals().dark_mode {
        Color32::from_rgb(120, 200, 120)
    } else {
        Color32::from_rgb(20, 120, 40)
    };
    let no_marks = Marks {
        ranges: &[],
        active: None,
    };
    let mut job = LayoutJob::default();
    let mut next = 0;
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let color = if line.starts_with("@@") {
            ui.visuals().weak_text_color()
        } else if line.starts_with('+') {
            added
        } else if line.starts_with('-') {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().text_color()
        };
        let format = TextFormat::simple(font.clone(), color);
        append_marked(
            &mut job,
            line,
            start,
            marks.unwrap_or(&no_marks),
            &mut next,
            &format,
        );
        start += line.len();
    }
    code_frame(ui, |ui| match marks {
        Some(marks) => show_marked(ui, job, text, marks, scroll),
        None => {
            ui.add(Label::new(job).wrap());
        }
    });
}

/// A wrapped label with its find matches highlighted.
fn marked_label(
    ui: &mut egui::Ui,
    text: &str,
    font: &FontId,
    color: Color32,
    marks: Option<&Marks>,
    scroll: bool,
) {
    let Some(marks) = marks else {
        ui.add(Label::new(RichText::new(text).font(font.clone()).color(color)).wrap());
        return;
    };
    let mut job = LayoutJob::default();
    let format = TextFormat::simple(font.clone(), color);
    append_marked(&mut job, text, 0, marks, &mut 0, &format);
    show_marked(ui, job, text, marks, scroll);
}

/// Shows a highlighted label, and scrolls to its current match when asked.
fn show_marked(ui: &mut egui::Ui, mut job: LayoutJob, text: &str, marks: &Marks, scroll: bool) {
    job.wrap.max_width = ui.available_width();
    let label = ui.add(Label::new(job.clone()).wrap());
    if scroll && let Some(range) = marks.active.and_then(|index| marks.ranges.get(index)) {
        // The same job and width give the label's galley back from the font cache.
        let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
        let cursor = CCursor::new(text[..range.start].chars().count());
        let rect = galley
            .pos_from_cursor(cursor)
            .translate(label.rect.min.to_vec2());
        ui.scroll_to_rect(rect, Some(Align::Center));
    }
}

/// Appends `segment`, which starts at byte `start` of the marked text, with its matches highlighted.
/// `next` is the first match not yet appended in full, carried from one segment to the next.
fn append_marked(
    job: &mut LayoutJob,
    segment: &str,
    start: usize,
    marks: &Marks,
    next: &mut usize,
    format: &TextFormat,
) {
    let end = start + segment.len();
    let mut position = start;
    while let Some(range) = marks.ranges.get(*next) {
        if range.start >= end {
            break;
        }
        let (from, to) = (range.start.max(position), range.end.min(end));
        if from > position {
            job.append(
                &segment[position - start..from - start],
                0.0,
                format.clone(),
            );
        }
        if to > from {
            let background = if marks.active == Some(*next) {
                ACTIVE_BG
            } else {
                MATCH_BG
            };
            let marked = TextFormat {
                background,
                ..format.clone()
            };
            job.append(&segment[from - start..to - start], 0.0, marked);
        }
        position = position.max(to);
        if range.end > end {
            break;
        }
        *next += 1;
    }
    if end > position {
        job.append(&segment[position - start..], 0.0, format.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: i64, text: &str, subagent: bool) -> Thread {
        Thread {
            id,
            parent_kind: subagent.then_some(0),
            search_text: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn filters_threads_by_words_hits_and_subagents() {
        let threads = [
            thread(1, "fix the build\n/src/app", false),
            thread(2, "explore the code", true),
            thread(3, "write docs", false),
        ];
        assert_eq!(filter_threads(&threads, "", false, None), [0, 2]);
        assert_eq!(filter_threads(&threads, "", true, None), [0, 1, 2]);
        assert_eq!(filter_threads(&threads, "BUILD src", false, None), [0]);
        let hits = HashSet::from([3]);
        assert_eq!(filter_threads(&threads, "cargo", false, Some(&hits)), [2]);
        assert_eq!(
            filter_threads(&threads, "the", true, Some(&hits)),
            [0, 1, 2]
        );
    }

    #[test]
    fn marks_matches_across_segments() {
        let text = "ab cd\nef gh\n";
        let ranges = [1..2, 4..8, 10..11];
        let marks = Marks {
            ranges: &ranges,
            active: Some(1),
        };
        let format = TextFormat::default();
        let mut job = LayoutJob::default();
        let mut next = 0;
        append_marked(&mut job, "ab cd\n", 0, &marks, &mut next, &format);
        append_marked(&mut job, "ef gh\n", 6, &marks, &mut next, &format);
        assert_eq!(job.text, text);
        let pieces: Vec<_> = job
            .sections
            .iter()
            .map(|section| {
                let kind = match section.format.background {
                    ACTIVE_BG => "active",
                    MATCH_BG => "match",
                    _ => "plain",
                };
                (
                    &job.text[section.byte_range.start.0..section.byte_range.end.0],
                    kind,
                )
            })
            .collect();
        assert_eq!(
            pieces,
            [
                ("a", "plain"),
                ("b", "match"),
                (" c", "plain"),
                ("d\nef", "active"),
                (" g", "plain"),
                ("h", "match"),
                ("\n", "plain"),
            ]
        );
    }
}
