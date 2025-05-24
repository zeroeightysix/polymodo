use crate::fuzzy_search::{FuzzySearch, Row};
use crate::windowing::app::{App, AppSender, AppSetup};
use crate::windowing::surface::LayerSurfaceOptions;
use crate::xdg::find_desktop_entries;
use anyhow::anyhow;
use egui::{Color32, CornerRadius, Response, Widget};
use egui_virtual_list::VirtualList;
use nucleo::Utf32String;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

static DESKTOP_ENTRIES: Mutex<Vec<SearchRow>> = Mutex::new(Vec::new());

fn copy_desktop_entry_cache() -> Vec<SearchRow> {
    let rows = DESKTOP_ENTRIES.lock().unwrap();

    rows.clone()
}

fn scour_desktop_entries(pusher: impl Fn(SearchRow)) {
    // immediately push cached entries
    {
        let rows = DESKTOP_ENTRIES.lock().unwrap();
        for row in &*rows {
            pusher(row.clone())
        }
    }

    // then start a search for new ones
    let entries = find_desktop_entries().collect::<Vec<_>>();
    // and add any new ones to the searcher
    {
        let mut rows = DESKTOP_ENTRIES.lock().unwrap();
        for entry in entries {
            let Some(exec) = entry.exec else {
                continue;
            };

            // an entry with `NoDisplay=true` does not qualify to be shown in the launcher
            if entry.no_display == Some(true) {
                continue;
            }

            // if, for this desktop entry, there exists no SearchRow yet (with comparison being done on the source path)
            if !rows.iter().any(|row| entry.source_path == row.path()) {
                log::debug!("new entry {}", entry.source_path.to_string_lossy(),);

                // add a new search entry for this desktop entry.
                let launcher_entry = Arc::new(LauncherEntry {
                    name: entry.name,
                    path: entry.source_path,
                    exec,
                    icon: entry.icon,
                    icon_resolved: OnceLock::new(),
                });

                // try locating the icon for this desktop entry, if any, and which may have to be deferred:
                if let Some(icon) = launcher_entry.icon.as_deref() {
                    let launcher_entry = launcher_entry.clone();

                    // if `Icon` is an absolute path, the image pointed at should be loaded:
                    if icon.starts_with('/') && std::fs::exists(icon).unwrap_or(false) {
                        let icon = format!("file://{icon}");

                        let _ = launcher_entry.icon_resolved.set(icon);
                    } else {
                        let icon = icon.to_string();
                        drop(tokio::task::spawn_blocking(move || {
                            // find the icon according to the spec:
                            let icon_path = linicon::lookup_icon(icon)
                                .with_scale(1) // TODO: use the surface scale
                                .with_size(32) // TODO: not sensible
                                .filter_map(Result::ok)
                                .next();

                            if let Some(icon_path) = icon_path {
                                let path = icon_path.path.to_string_lossy().to_string();
                                let path = format!("file://{path}");

                                let _ = launcher_entry.icon_resolved.set(path);
                            }
                        }));
                    }
                }

                rows.push(SearchRow(launcher_entry));

                // and also add it to the fuzzy searcher
                let entry = rows.last().unwrap().clone();
                pusher(entry);
            }
        }
    }
}

pub struct Launcher {
    search_input: String,
    focus_search: bool,
    search: FuzzySearch<1, SearchRow>,
    results: Vec<SearchRow>,
    selected_entry_idx: usize,
    list: VirtualList,
    finish: Option<tokio::sync::oneshot::Sender<<Self as App>::Output>>,
}

#[derive(Debug, Clone)]
struct LauncherEntry {
    name: String,
    path: PathBuf,
    exec: String,
    icon: Option<String>,
    icon_resolved: OnceLock<String>,
}

impl Launcher {
    pub fn layer_surface_options() -> LayerSurfaceOptions<'static> {
        LayerSurfaceOptions {
            namespace: Some("polymodo"),
            width: 500,
            height: 600,
            ..Default::default()
        }
    }

    fn app_launcher_ui(&mut self, ui: &mut egui::Ui) {
        let text_edit_rsp = self.show_search_text_edit(ui);

        // if the text input has changed,
        if text_edit_rsp.changed() {
            // make a new search.
            self.search.search::<0>(self.search_input.as_str());
            // and reset the selection
            // TODO: perhaps if in the new search result, the selected item persist,
            // adjust the selection to "follow" it.
            self.selected_entry_idx = 0;
        }

        // if there's no results, don't show anything.
        if self.results.is_empty() {
            return;
        }

        ui.separator();

        self.show_results(ui);

        let _scroll = {
            // if up/down has been pressed, adjust the selected entry
            if ui.input(|input| input.key_pressed(egui::Key::ArrowDown)) {
                self.selected_entry_idx = (self.selected_entry_idx + 1) % self.results.len();

                true
            } else if ui.input(|input| input.key_pressed(egui::Key::ArrowUp)) {
                if self.selected_entry_idx == 0 {
                    self.selected_entry_idx = self.results.len() - 1;
                } else {
                    self.selected_entry_idx =
                        (self.selected_entry_idx.saturating_sub(1)) % self.results.len();
                }

                true
            } else {
                // the selected entry didn't change, so we shouldn't scroll to its row.
                false
            }
        };

        // if enter was pressed (within the textedit)
        if text_edit_rsp.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
            && !self.results.is_empty()
        {
            let entry = self.results.get(self.selected_entry_idx);
            if let Some(entry) = entry.map(|e| e.0.as_ref()) {
                if let Err(e) = launch(entry) {
                    log::error!("failed to launch with error {e}");
                }

                self.finish();
            }
        }
    }

    fn show_search_text_edit(&mut self, ui: &mut egui::Ui) -> Response {
        ui.horizontal(|ui| {
            ui.label("🔍"); // TODO: vertical center (line_height = row_height + margin)

            let text_edit = egui::TextEdit::singleline(&mut self.search_input)
                .desired_width(f32::INFINITY)
                .frame(false)
                .hint_text("Search")
                .show(ui)
                .response;

            if std::mem::replace(&mut self.focus_search, false) {
                text_edit.request_focus();
            }

            text_edit
        })
        .inner
    }

    fn show_results(&mut self, ui: &mut egui::Ui) {
        let results = &self.results;
        let list = &mut self.list;

        egui::ScrollArea::vertical()
            .min_scrolled_height(500.0) // TODO: ui.available_height() is 0; why?
            .show(ui, |ui| {
                ui.set_width(ui.available_width());

                list.ui_custom_layout(ui, results.len(), |ui, start_idx| {
                    let mut items_shown = 0;
                    #[allow(clippy::needless_range_loop)]
                    for idx in start_idx..results.len() {
                        let result = &results[idx];

                        fn display_result(result: &SearchRow, ui: &mut egui::Ui) {
                            ui.horizontal(|ui| {
                                if let Some(icon) = result.icon() {
                                    egui::Image::new(icon)
                                        .fit_to_exact_size(egui::Vec2::splat(32.0))
                                        .ui(ui);
                                }

                                ui.label(result.name());
                            });
                        }

                        if self.selected_entry_idx == idx {
                            egui::Frame::new()
                                .fill(Color32::from_black_alpha(40))
                                .outer_margin(egui::Margin::same(-2))
                                .inner_margin(egui::Margin::same(2))
                                .corner_radius(8.0)
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    display_result(result, ui);
                                });
                        } else {
                            display_result(result, ui);
                        }

                        items_shown += 1;

                        if ui.available_height() <= 0.0 {
                            break;
                        }
                    }

                    items_shown
                });
            });
    }

    fn finish(&mut self) {
        if let Some(finish) = self.finish.take() {
            // this cannot reasonably ever fail;
            // that'd mean the `AppSetup`'s effects have been dropped,
            // as the receiving end of the finish sender is held there.
            let _ = finish.send(Ok(()));
        } else {
            log::warn!("tried to finish App, but such a message was already sent")
        }
    }
}

impl App for Launcher {
    type Message = Message;
    type Output = anyhow::Result<()>;

    fn create(message_sender: AppSender<Self::Message>) -> AppSetup<Self, Self::Output> {
        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let search = FuzzySearch::create_with_config(config);
        let pusher = search.pusher();

        let (finish, finish_recv) = tokio::sync::oneshot::channel();

        let entries = copy_desktop_entry_cache();

        tokio::task::spawn_blocking(move || scour_desktop_entries(pusher));

        let launcher = Launcher {
            search_input: String::new(),
            focus_search: true,
            // desktop_entries,
            search,
            results: entries,
            selected_entry_idx: 0,
            list: Default::default(),
            finish: Some(finish),
        };

        let notify = launcher.search.notify();

        AppSetup::new(launcher)
            // effect for searching
            .spawn_local(async move {
                loop {
                    notify.notified().await;

                    let _ = message_sender.send(Message::Search);
                }
            })
            // output effect
            .spawn_local(async move {
                let result = finish_recv.await?;

                log::info!("finish! {result:?}");

                result
            })
    }

    fn on_message(&mut self, message: Self::Message) {
        match message {
            Message::Search => {
                self.search.tick();
                self.results = self.search.get_matches().into_iter().cloned().collect();
            }
        }
    }

    fn render(&mut self, ctx: &egui::Context) {
        let mut frame = egui::Frame::window(&ctx.style());
        frame.shadow.offset[1] = frame.shadow.offset[0];
        frame.fill = Color32::from_black_alpha(210);
        frame.inner_margin = egui::Margin::same(8);
        frame.corner_radius = CornerRadius::same(16);

        egui::TopBottomPanel::top("top_panel")
            .max_height(600.0)
            .frame(frame.outer_margin((frame.shadow.blur + frame.shadow.spread + 1) as f32))
            .show_separator_line(false)
            .show(ctx, |ui| {
                self.app_launcher_ui(ui);
            });

        // Exit when escape is pressed
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.finish();
        }
    }
}

fn launch(entry: &LauncherEntry) -> anyhow::Result<()> {
    match fork::fork().map_err(|_| anyhow!("failed to fork process"))? {
        fork::Fork::Child => {
            // detach
            if let Err(e) = fork::setsid() {
                log::error!("setsid failed: {}", e);
            }
            if let Err(e) = chdir() {
                log::error!("chdir failed: {}", e);
            }

            // %f and %F: lists of files. polymodo does not yet support selecting files.
            let exec = entry.exec.replace("%f", "").replace("%F", "");
            // same story for %u and %U:
            let exec = exec.replace("%u", "").replace("%U", "");

            // split exec by spaces
            let mut args = exec
                .split(" ")
                .flat_map(|arg| match arg {
                    "%i" => vec!["--icon", entry.icon.as_deref().unwrap_or("")],
                    "%c" => vec![entry.name.as_str()],
                    "%k" => {
                        vec![entry.path.as_os_str().to_str().unwrap_or("")]
                    }
                    // remove empty strings as arguments; these may be left over from
                    //   trailing/subsequent whitespaces, and cause programs to misbehave.
                    "" => {
                        vec![]
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
        fork::Fork::Parent(pid) => {
            log::info!("Launching {:?} with pid {pid}", entry.name.as_str());
            let _ = std::io::stdout().flush();
            Ok(())
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

/// Arc around a [LauncherEntry], meant to be shareable between the fuzzy matcher and UI.
#[derive(Clone, Debug)]
struct SearchRow(pub Arc<LauncherEntry>);

impl Row<1> for SearchRow {
    type Output = Utf32String;

    fn columns(&self) -> [Self::Output; 1] {
        [self.name().into()]
    }
}

impl SearchRow {
    fn name(&self) -> &str {
        self.0.name.as_str()
    }

    fn icon(&self) -> Option<&str> {
        self.0.icon_resolved.get().map(|s| s.as_str())
    }

    fn path(&self) -> &Path {
        &self.0.path
    }
}
