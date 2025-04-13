use crate::fuzzy_search::{FuzzySearch, Row};
use crate::windowing::app::{App, AppSender, AppSetup};
use crate::windowing::surface::LayerSurfaceOptions;
use crate::xdg::DesktopEntry;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use egui::RichText;
use egui_extras::{Column, TableBuilder};

fn desktop_entries() -> &'static Vec<SearchRow> {
    static DESKTOP_ENTRIES: OnceLock<Vec<SearchRow>> = OnceLock::new();
    DESKTOP_ENTRIES.get_or_init(|| {
        crate::xdg::find_desktop_entries()
            .into_iter()
            .map(|v| SearchRow(&*Box::leak(Box::new(v))))
            .collect()
    })
}

pub struct Launcher {
    search_input: String,
    focus_search: bool,
    search: FuzzySearch<1, SearchRow>,
    show_entries: Vec<SearchRow>,
    selected_entry_idx: usize,
}

impl Launcher {
    pub fn layer_surface_options() -> LayerSurfaceOptions<'static> {
        LayerSurfaceOptions {
            namespace: Some("polymodo"),
            width: 350,
            height: 400,
            ..Default::default()
        }
    }

    fn app_launcher_ui(&mut self, ui: &mut egui::Ui) {
        let response = egui::TextEdit::singleline(&mut self.search_input)
            .desired_width(f32::INFINITY)
            .show(ui)
            .response;
        if std::mem::replace(&mut self.focus_search, false) {
            response.request_focus();
        }

        let scroll = if response.has_focus() {
            // if up/down has been pressed, adjust the selected entry
            if ui.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                self.selected_entry_idx = (self.selected_entry_idx + 1) % self.show_entries.len();

                true
            } else if ui.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                self.selected_entry_idx =
                    (self.selected_entry_idx.saturating_sub(1)) % self.show_entries.len();

                true
            } else {
                // the selected entry didn't change, so we shouldn't scroll to its row.
                false
            }
        } else {
            false
        };

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
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
            && !self.show_entries.is_empty()
        {
            let entry = self.show_entries.get(self.selected_entry_idx);
            if let Some(entry) = entry.map(|e| e.0) {
                launch(&entry);
            }
        }

        let remainder = ui.available_height();
        let row_height = ui.text_style_height(&egui::TextStyle::Body);

        let mut table = TableBuilder::new(ui)
            .column(Column::remainder())
            .animate_scrolling(false)
            .min_scrolled_height(remainder);
        if scroll {
            table = table.scroll_to_row(self.selected_entry_idx, None);
        }
        table.body(|body| {
            body.rows(row_height, self.show_entries.len(), |mut row| {
                let idx = row.index();
                let entry = self.show_entries[idx];
                let checked = self.selected_entry_idx == idx;
                row.col(|ui| {
                    let mut text = RichText::new(entry.name());
                    if checked {
                        text = text.strong();
                    }


                    let label = ui.label(text);
                    if label.clicked()
                    {
                        self.selected_entry_idx = idx;
                    }
                    if label.hovered() {
                        label.highlight();
                    }
                });
            })
        });
    }
}

impl App for Launcher {
    type Message = Message;
    type Output = ();

    fn create(message_sender: AppSender<Self::Message>) -> AppSetup<Self, Self::Output> {
        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let search = FuzzySearch::create_with_config(config);
        let pusher = search.pusher();

        tokio::task::spawn_blocking(move || {
            let desktop_entries: Vec<_> = desktop_entries().to_vec();
            for entry in desktop_entries {
                pusher(entry);
            }
        });

        let launcher = Launcher {
            search_input: String::new(),
            focus_search: true,
            // desktop_entries,
            search,
            show_entries: Vec::new(),
            selected_entry_idx: 0,
        };

        let notify = launcher.search.notify();

        AppSetup::new(launcher)
            // effect for searching
            .spawn_local(async move {
                loop {
                    notify.notified().await;

                    message_sender.send(Message::Search).unwrap()
                }
            })
    }

    fn on_message(&mut self, message: Self::Message) {
        match message {
            Message::Search => {
                self.search.tick();
                self.show_entries = self.search.get_matches().into_iter().copied().collect();
            }
        }
    }

    fn render(&mut self, ctx: &egui::Context) {
        let mut frame = egui::Frame::window(&ctx.style());
        frame.shadow.offset[1] = frame.shadow.offset[0];

        egui::CentralPanel::default()
            .frame(frame.outer_margin((frame.shadow.blur + frame.shadow.spread + 1) as f32))
            .show(ctx, |ui| {
                self.app_launcher_ui(ui);
            });

        // Kill when escape is pressed
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            std::process::exit(0);
        }
    }
}

fn launch(entry: &DesktopEntry) {
    let Some(exec) = &entry.exec else {
        return;
    };

    match fork::fork() {
        Ok(fork::Fork::Child) => {
            // detach
            if let Err(e) = fork::setsid() {
                log::error!("setsid failed: {}", e);
            }
            if let Err(e) = chdir() {
                log::error!("chdir failed: {}", e);
            }

            // %f and %F: lists of files. polymodo does not yet support selecting files.
            let exec = exec.replace("%f", "").replace("%F", "");
            // same story for %u and %U:
            let exec = exec.replace("%u", "").replace("%U", "");

            // split exec by spaces
            let mut args = exec
                .split(" ")
                .flat_map(|arg| match arg {
                    "%i" => vec!["--icon", entry.icon.as_deref().unwrap_or("")],
                    "%c" => vec![entry.name.as_str()],
                    "%k" => {
                        vec![entry.source_path.as_os_str().to_str().unwrap_or("")]
                    }
                    _ => vec![arg],
                })
                .collect::<Vec<_>>();
            // the first "argument" is the program to launch
            let program = args.remove(0);

            log::debug!("launching: prog='{}' args='{}'", program, args.join(" "));

            let error = Command::new(program).args(args).exec(); // this will never return if the exec succeeds

            // but if it did return, log the error and return:
            log::error!("failed to launch: {}", error);
            let _ = std::io::stdout().flush();
            std::process::exit(-1);
        }
        Ok(fork::Fork::Parent(pid)) => {
            log::info!("Launching {:?} with pid {pid}", entry.name.as_str());
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


fn chdir() -> std::io::Result<()> {
    let home = home::home_dir().unwrap_or(PathBuf::from("/"));
    std::env::set_current_dir(&home)
}

#[derive(Debug, Clone)]
pub enum Message {
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
