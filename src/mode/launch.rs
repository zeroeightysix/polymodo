use crate::fuzzy_search::{FuzzySearch, Row};
use crate::xdg::DesktopEntry;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Command;
use windowing::egui;
use windowing::egui::{Frame, Key, ScrollArea, TextEdit, TextStyle, Ui};

pub struct Launcher {
    search_input: String,
    focus_search: bool,
    search: FuzzySearch<1, SearchRow>,
    show_entries: Vec<SearchRow>,
    selected_entry_idx: usize,
}

impl Launcher {
    pub fn create() -> Self {
        let desktop_entries: Vec<_> = crate::xdg::find_desktop_entries()
            .into_iter()
            .map(|v| SearchRow(&*Box::leak(Box::new(v))))
            .collect();

        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let search = FuzzySearch::create_with_config(config);
        search.push_all(desktop_entries);

        Launcher {
            search_input: String::new(),
            focus_search: true,
            // desktop_entries,
            search,
            show_entries: Vec::new(),
            selected_entry_idx: 0,
        }
    }

    fn app_launcher_ui(&mut self, ui: &mut Ui) {
        let response = TextEdit::singleline(&mut self.search_input)
            .desired_width(f32::INFINITY)
            .show(ui)
            .response;
        if std::mem::replace(&mut self.focus_search, false) {
            response.request_focus();
        }

        if response.has_focus() {
            // if up/down has been pressed, adjust the selected entry
            if ui.input(|input| input.key_pressed(Key::ArrowDown)) {
                self.selected_entry_idx = (self.selected_entry_idx + 1) % self.show_entries.len();
            } else if ui.input(|input| input.key_pressed(Key::ArrowUp)) {
                self.selected_entry_idx =
                    (self.selected_entry_idx.saturating_sub(1)) % self.show_entries.len();
            }
        }
        // if the text input has changed,
        if response.changed() {
            // make a new search.
            self.search.search::<0>(self.search_input.as_str());
            // and reset the selection
            // TODO: perhaps if in the new search result, the selected item persist,
            // adjust the selection to "follow" it.
            self.selected_entry_idx = 0;
        }
        // if enter was pressed (within the textedit)
        if response.lost_focus()
            && ui.input(|i| i.key_pressed(Key::Enter))
            && !self.show_entries.is_empty()
        {
            let entry = self.show_entries.get(self.selected_entry_idx);
            if let Some(exec) = entry.and_then(|e| e.0.exec.as_ref()) {
                match fork::fork() {
                    Ok(fork::Fork::Child) => {
                        // detach
                        fork::setsid().unwrap();

                        // TODO: Exec key is a lot more complex than this!
                        // split exec by spaces
                        let mut args = exec.split(" ").collect::<Vec<_>>();
                        // the first "argument" is the program to launch
                        let program = args.remove(0);

                        let error = Command::new(program).args(args).exec(); // this will never return if the exec succeeds

                        // but if it did return, log the error and return:
                        log::error!("failed to launch: {}", error);
                        let _ = std::io::stdout().flush();
                        std::process::exit(-1);
                    }
                    Ok(fork::Fork::Parent(pid)) => {
                        log::info!("Launched {:?} with pid {pid}", entry.map(|e| e.name()));
                        let _ = std::io::stdout().flush();
                        std::process::exit(0);
                    }
                    Err(e) => {
                        log::error!("Fork failed: {}", e);
                        let _ = std::io::stdout().flush();
                        std::process::exit(-1);
                    }
                }
            }
        }

        let row_height = ui.text_style_height(&TextStyle::Monospace);
        ScrollArea::vertical().auto_shrink(false).show_rows(
            ui,
            row_height,
            self.show_entries.len(),
            |ui, rows| {
                for row in rows {
                    let entry = self.show_entries[row];
                    if ui
                        .selectable_label(self.selected_entry_idx == row, entry.name())
                        .clicked()
                    {
                        self.selected_entry_idx = row;
                    }
                }
            },
        );
    }

    fn on_message(&mut self, message: Message) {
        match message {
            Message::Search => {
                self.search.tick();
                self.show_entries = self.search.get_matches().into_iter().copied().collect();
            }
        }
    }
}

impl windowing::app::App for Launcher {
    fn render(&mut self, ctx: &egui::Context) {
        let mut frame = Frame::window(&ctx.style());
        frame.shadow.offset[1] = frame.shadow.offset[0];

        egui::CentralPanel::default()
            .frame(frame.outer_margin((frame.shadow.blur + frame.shadow.spread + 1) as f32))
            .show(ctx, |ui| {
                self.app_launcher_ui(ui);
            });

        // Kill when escape is pressed
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            std::process::exit(0);
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Search,
}

#[derive(Copy, Clone, Debug)]
struct SearchRow(pub &'static DesktopEntry);

impl Row<1> for SearchRow {
    type Output = &'static str;

    fn columns(&self) -> [Self::Output; 1] {
        [self.name()]
    }
}

impl SearchRow {
    fn name(&self) -> &'static str {
        self.0.name()
    }
}
