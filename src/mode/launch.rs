use std::collections::HashMap;
use crate::fuzzy_search::{FuzzySearch, Row};
use crate::windowing::app::{App, AppSender, AppSetup, SurfaceEvent};
use crate::windowing::surface::LayerSurfaceOptions;
use crate::xdg::find_desktop_entries;
use anyhow::anyhow;
use egui::{Color32, CornerRadius, FontId, Response, RichText, Widget};
use nucleo::Utf32String;
use smithay_client_toolkit::shell::wlr_layer::Layer;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;
use icon::Icons;
use tokio::sync::mpsc;

static DESKTOP_ENTRIES: Mutex<Vec<SearchRow>> = Mutex::new(Vec::new());
static ICONS: LazyLock<Icons> = LazyLock::new(Icons::new);

type LaunchHistory = HashMap<PathBuf, u32>;

#[derive(Debug, Default, bincode::Decode, bincode::Encode)]
struct LauncherEntryBiasState {
    history: LaunchHistory,
}

fn copy_desktop_entry_cache() -> Vec<SearchRow> {
    let rows = DESKTOP_ENTRIES.lock().unwrap();

    rows.clone()
}

struct IconWorker {
    sender: mpsc::UnboundedSender<Arc<LauncherEntry>>,
}

fn scour_desktop_entries(pusher: impl Fn(SearchRow), history: &LaunchHistory) {
    // immediately push cached entries
    {
        let rows = DESKTOP_ENTRIES.lock().unwrap();
        for row in &*rows {
            pusher(row.clone())
        }
    }

    // then start a search for new ones
    let start = Instant::now();
    let entries = find_desktop_entries();
    // and add any new ones to the searcher
    {
        let mut rows = DESKTOP_ENTRIES.lock().unwrap();
        let mut new_entries = 0u32;

        let mut icon_worker: Option<IconWorker> = None;

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
                log::trace!("new entry {}", entry.source_path.to_string_lossy(),);
                new_entries += 1;

                // add a new search entry for this desktop entry.
                let launcher_entry = Arc::new(LauncherEntry {
                    name: entry.name,
                    path: entry.source_path,
                    exec,
                    icon: entry.icon,
                    icon_resolved: OnceLock::new(),
                });

                // try locating the icon for this desktop entry, if any, and which may have to be deferred:
                let worker = icon_worker.get_or_insert_with(|| {
                    let (sender, mut receiver) = mpsc::unbounded_channel();
                    let _handle = tokio::task::spawn_blocking(move || -> Option<()> {
                        loop {
                            let entry = receiver.blocking_recv()?;

                            find_and_set_icon(&entry);
                        }
                    });

                    IconWorker {
                        sender,
                    }
                });

                let _ = worker.sender.send(launcher_entry.clone());

                let bonus_score = history.get(&launcher_entry.path).cloned().unwrap_or(0);

                rows.push(SearchRow {
                    entry: launcher_entry,
                    bonus_score
                });

                // and also add it to the fuzzy searcher
                let entry = rows.last().unwrap().clone();
                pusher(entry)
            }
        }

        if new_entries != 0 {
            let time_it_took = Instant::now() - start;

            log::debug!("Took {time_it_took:?} to find {new_entries} new entries");
        }
    }
}

fn find_and_set_icon(launcher_entry: &Arc<LauncherEntry>) {
    let launcher_entry = launcher_entry.clone();

    let Some(icon) = launcher_entry.icon.as_ref() else {
        return;
    };

    // if `Icon` is an absolute path, the image pointed at should be loaded:
    if icon.starts_with('/') && std::fs::exists(icon).unwrap_or(false) {
        let icon = format!("file://{icon}");

        let _ = launcher_entry.icon_resolved.set(icon);
    } else {
        let icon = icon.to_string();
        let icon = ICONS.find_icon(icon.as_str(), 32, 1, "Adwaita"); // TODO: find user icon theme

        if let Some(icon) = icon {
            let path = icon.path.to_string_lossy().to_string();
            let path = format!("file://{path}");

            let _ = launcher_entry.icon_resolved.set(path);
        }
    }
}

pub struct Launcher {
    search_input: String,
    focus_search: bool,
    search: FuzzySearch<1, SearchRow>,
    results: Vec<SearchRow>,
    selected_entry_idx: usize,
    finish: Option<tokio::sync::oneshot::Sender<<Self as App>::Output>>,
    bias: LauncherEntryBiasState,
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
            layer: Layer::Overlay,
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

        ui.separator();

        // if there's no results, don't show anything.
        if self.results.is_empty() {
            return;
        }

        let scroll = {
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

        self.show_results(ui, scroll);

        // if enter was pressed (within the textedit)
        if text_edit_rsp.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
            && !self.results.is_empty()
        {
            let entry = self.results.get(self.selected_entry_idx);
            if let Some(entry) = entry.map(|e| e.entry.as_ref()) {
                // boost the bias for this entry and 'demote' others
                let bias = &mut self.bias;
                bias.history.values_mut()
                    .for_each(|avg| *avg = decrement_history_value(*avg));

                let this_entry = bias.history.entry(entry.path.clone())
                    .or_default();
                *this_entry = bump_history_value(*this_entry);

                if let Err(e) = crate::persistence::write_state("launcher", "entry_bias", &*bias) {
                    log::error!("failed to save history state: {e}");
                }

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

    fn show_results(&mut self, ui: &mut egui::Ui, scroll: bool) {
        const ICON_SIZE: f32 = 32.0;
        
        let results = &self.results;
        let available_height = ui.available_height();

        let mut area = egui::ScrollArea::both()
            .min_scrolled_height(available_height)
            .min_scrolled_width(ui.available_width());
        
        if scroll {
            let row_height_with_spacing = ICON_SIZE + ui.spacing().item_spacing.y;
            let scroll_offset = (self.selected_entry_idx as f32) * row_height_with_spacing - ui.spacing().item_spacing.y;

            let window_rect = ui.ctx().input(|i: &egui::InputState| i.screen_rect());
            let window_height: f32 = window_rect.max[1] - window_rect.min[1];

            let offset = scroll_offset - window_height * 0.2;
            // clamp to the actual visible height
            let max_offset = results.len() as f32 * row_height_with_spacing - available_height - ui.spacing().item_spacing.y;
            let offset = offset.clamp(
                0.0,
                max_offset.max(0.0),
            );

            area = area.vertical_scroll_offset(offset);
        }
        
        area
            .show_rows(ui, ICON_SIZE, results.len(), |ui, range| {
                ui.set_width(ui.available_width());

                for idx in range {
                    let result = &results[idx];
                    fn display_result(result: &SearchRow, ui: &mut egui::Ui) {
                        ui.horizontal_centered(|ui| {
                            ui.set_height(ui.available_height());

                            if let Some(icon) = result.icon() {
                                egui::Image::new(icon)
                                    .fit_to_exact_size(egui::Vec2::splat(ICON_SIZE))
                                    .ui(ui);
                            } else {
                                ui.add_space(ICON_SIZE + ui.spacing().item_spacing.x);
                            }

                            ui.label(
                                RichText::new(result.name())
                                    .font(FontId::proportional(ICON_SIZE - 8.0)),
                            );
                        });
                    }

                    // fixed height for a row
                    ui.scope(|ui| {
                        ui.set_height(ICON_SIZE);

                        if self.selected_entry_idx == idx {
                            egui::Frame::new()
                                .fill(Color32::from_gray(64))
                                .outer_margin(egui::Margin::same(-2))
                                .inner_margin(egui::Margin::same(2))
                                .corner_radius(4.0)
                                .show(ui, |ui| {
                                    ui.set_height(ui.available_height());
                                    ui.set_width(ui.available_width());
                                    display_result(result, ui);
                                });
                        } else {
                            display_result(result, ui);
                        }
                    });
                }
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
        // read the bias from persistent state, if any.
        let bias: LauncherEntryBiasState = crate::persistence::read_state("launcher", "entry_bias")
            .ok()
            .unwrap_or_default();

        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let search = FuzzySearch::create_with_config(config);
        let pusher = search.pusher();

        let (finish, finish_recv) = tokio::sync::oneshot::channel();

        let entries = copy_desktop_entry_cache();

        {
            // TODO: avoid clone, bias should go through FuzzySearch instead
            let bias = bias.history.clone();
            tokio::task::spawn_blocking(move || scour_desktop_entries(pusher, &bias));
        }

        let launcher = Launcher {
            search_input: String::new(),
            focus_search: true,
            // desktop_entries,
            search,
            results: entries,
            selected_entry_idx: 0,
            finish: Some(finish),
            bias,
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

    fn on_surface_event(&mut self, surface_event: SurfaceEvent) {
        #[allow(clippy::single_match)]
        match surface_event {
            SurfaceEvent::KeyboardLeave(_) => self.finish(),
            _ => {}
        }
    }

    fn render(&mut self, ctx: &egui::Context) {
        let mut frame = egui::Frame::window(&ctx.style());
        frame.shadow.offset[1] = frame.shadow.offset[0];
        frame.fill = Color32::from_black_alpha(210);
        frame.inner_margin = egui::Margin::same(8);
        frame.corner_radius = CornerRadius::same(16);

        egui::CentralPanel::default()
            .frame(frame.outer_margin((frame.shadow.blur + frame.shadow.spread + 1) as f32))
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
            if let Err(e) = nix::unistd::daemon(false, false) {
                log::error!("daemonize failed: {}", e);
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

fn bump_history_value(value: u32) -> u32 {
    const ALPHA: f32 = 0.5f32;
    const INV_ALPHA: f32 = 1f32 - ALPHA;
    let increment = 100;

    (ALPHA * increment as f32 + INV_ALPHA * value as f32) as u32
}

fn decrement_history_value(value: u32) -> u32 {
    const ALPHA: f32 = 0.1f32;
    const INV_ALPHA: f32 = 1f32 - ALPHA;
    let increment = 0;

    (ALPHA * increment as f32 + INV_ALPHA * value as f32) as u32
}

#[derive(Debug, Clone)]
pub enum Message {
    Search,
}

/// Arc around a [LauncherEntry], meant to be shareable between the fuzzy matcher and UI.
#[derive(Clone, Debug)]
struct SearchRow {
    pub entry: Arc<LauncherEntry>,
    pub bonus_score: u32,
}

impl Row<1> for SearchRow {
    type Output = Utf32String;

    fn columns(&self) -> [Self::Output; 1] {
        [self.name().into()]
    }

    fn bonus(&self) -> u32 {
        self.bonus_score
    }
}

impl SearchRow {
    fn name(&self) -> &str {
        self.entry.name.as_str()
    }

    fn icon(&self) -> Option<&str> {
        self.entry.icon_resolved.get().map(|s| s.as_str())
    }

    fn path(&self) -> &Path {
        &self.entry.path
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test() {

    }
}
