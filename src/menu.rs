use std::sync::mpsc::{Receiver, channel};

use eframe::egui;

/// What a menu item asks for. The app runs it like the matching keyboard shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    ShowHistory,
    ShowThreads,
    ToggleSubagents,
    Find,
    SearchAll,
    FindNext,
    FindPrevious,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    OpenDocs,
}

impl Command {
    const ALL: [Self; 14] = [
        Self::ShowHistory,
        Self::ShowThreads,
        Self::ToggleSubagents,
        Self::Find,
        Self::SearchAll,
        Self::FindNext,
        Self::FindPrevious,
        Self::Undo,
        Self::Redo,
        Self::Cut,
        Self::Copy,
        Self::Paste,
        Self::SelectAll,
        Self::OpenDocs,
    ];

    /// The menu item id, which comes back with each click.
    fn id(self) -> String {
        format!("{self:?}")
    }

    fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|command| command.id() == id)
    }
}

/// The native menu bar on macOS. Elsewhere it has no items and sends no commands.
pub struct MenuBar {
    commands: Receiver<Command>,
    #[cfg(target_os = "macos")]
    native: Option<macos::Native>,
}

impl MenuBar {
    pub fn install(ctx: &egui::Context) -> Self {
        let (sender, commands) = channel();
        #[cfg(target_os = "macos")]
        let native = macos::Native::install(ctx.clone(), sender)
            .inspect_err(|err| eprintln!("Cannot install the menu bar: {err}"))
            .ok();
        #[cfg(not(target_os = "macos"))]
        let _ = (ctx, sender);
        Self {
            commands,
            #[cfg(target_os = "macos")]
            native,
        }
    }

    pub fn take_commands(&self) -> Vec<Command> {
        self.commands.try_iter().collect()
    }

    pub fn set_subagents(&self, shown: bool) {
        #[cfg(target_os = "macos")]
        if let Some(native) = &self.native {
            native.subagents.set_checked(shown);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = shown;
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::mpsc::Sender;

    use eframe::egui;
    use muda::{
        AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu,
        accelerator::Accelerator,
    };

    use super::Command;

    pub struct Native {
        /// The menu bar stays installed while this is kept.
        _menu: Menu,
        pub subagents: CheckMenuItem,
    }

    fn item(command: Command, text: &str, shortcut: Option<&str>) -> MenuItem {
        let accelerator = shortcut.and_then(|keys| keys.parse::<Accelerator>().ok());
        MenuItem::with_id(command.id(), text, true, accelerator)
    }

    impl Native {
        /// Replaces the menu that winit puts up at launch. The handler wakes egui, because a
        /// menu click is not an input event that would draw a new frame.
        pub fn install(ctx: egui::Context, sender: Sender<Command>) -> muda::Result<Self> {
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some(command) = Command::from_id(event.id().as_ref())
                    && sender.send(command).is_ok()
                {
                    ctx.request_repaint();
                }
            }));

            let about = AboutMetadata {
                name: Some("Aguin".into()),
                version: Some(env!("CARGO_PKG_VERSION").into()),
                comments: Some("A viewer for the shell history and agent threads of atuin.".into()),
                ..AboutMetadata::default()
            };
            let separator = PredefinedMenuItem::separator;
            let app = Submenu::with_items(
                "Aguin",
                true,
                &[
                    &PredefinedMenuItem::about(Some("About Aguin"), Some(about)),
                    &separator(),
                    &PredefinedMenuItem::services(None),
                    &separator(),
                    &PredefinedMenuItem::hide(Some("Hide Aguin")),
                    &PredefinedMenuItem::hide_others(None),
                    &PredefinedMenuItem::show_all(None),
                    &separator(),
                    &PredefinedMenuItem::quit(Some("Quit Aguin")),
                ],
            )?;
            let find = Submenu::with_items(
                "Find",
                true,
                &[
                    &item(Command::Find, "Find…", Some("Cmd+F")),
                    &item(
                        Command::SearchAll,
                        "Search All Threads…",
                        Some("Cmd+Shift+F"),
                    ),
                    &item(Command::FindNext, "Find Next", Some("Cmd+G")),
                    &item(Command::FindPrevious, "Find Previous", Some("Cmd+Shift+G")),
                ],
            )?;
            // egui draws its own text fields, so AppKit's copy: and paste: actions would reach
            // no responder: these items send commands that the app passes to egui.
            let edit = Submenu::with_items(
                "Edit",
                true,
                &[
                    &item(Command::Undo, "Undo", Some("Cmd+Z")),
                    &item(Command::Redo, "Redo", Some("Cmd+Shift+Z")),
                    &separator(),
                    &item(Command::Cut, "Cut", Some("Cmd+X")),
                    &item(Command::Copy, "Copy", Some("Cmd+C")),
                    &item(Command::Paste, "Paste", Some("Cmd+V")),
                    &item(Command::SelectAll, "Select All", Some("Cmd+A")),
                    &separator(),
                    &find,
                ],
            )?;
            let subagents = CheckMenuItem::with_id(
                Command::ToggleSubagents.id(),
                "Show Subagents",
                true,
                false,
                None,
            );
            let view = Submenu::with_items(
                "View",
                true,
                &[
                    &item(Command::ShowHistory, "Shell History", Some("Cmd+1")),
                    &item(Command::ShowThreads, "Agent Threads", Some("Cmd+2")),
                    &separator(),
                    &subagents,
                ],
            )?;
            let window = Submenu::with_items(
                "Window",
                true,
                &[
                    &PredefinedMenuItem::minimize(None),
                    &PredefinedMenuItem::maximize(Some("Zoom")),
                    &separator(),
                    &PredefinedMenuItem::bring_all_to_front(None),
                ],
            )?;
            let help = Submenu::with_items(
                "Help",
                true,
                &[&item(Command::OpenDocs, "Atuin Documentation", None)],
            )?;

            let menu = Menu::with_items(&[&app, &edit, &view, &window, &help])?;
            window.set_as_windows_menu_for_nsapp();
            help.set_as_help_menu_for_nsapp();
            menu.init_for_nsapp();
            Ok(Self {
                _menu: menu,
                subagents,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_its_own_id() {
        for command in Command::ALL {
            assert_eq!(Command::from_id(&command.id()), Some(command));
        }
        assert_eq!(Command::from_id("unknown"), None);
    }
}
