use crate::app::{App, AppName, AppSender};
use crate::fuzzy_search::{FuzzySearch, Row};
use crate::mode::{HideOnDrop, HideOnDropExt};
use crate::xdg::find_desktop_entries;
use crate::{main, ui};
use anyhow::anyhow;
use icon::Icons;
use nucleo::Utf32String;
use slint::{ComponentHandle, Model, ModelRc, Rgba8Pixel, VecModel};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;

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

// struct IconWorker {
//     sender: smol::channel::Sender<Arc<LauncherEntry>>,
//     task: smol::Task<Option<()>>,
// }

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

        // TODO: dropping this will cancel the work task
        // let mut icon_worker: Option<IconWorker> = None;

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
                // let worker = icon_worker.get_or_insert_with(|| {
                //     let (sender, receiver) = smol::channel::unbounded();
                //     let task = smol::unblock(move || -> Option<()> {
                //         loop {
                //             let entry = receiver.recv_blocking().ok()?;

                find_and_set_icon(&launcher_entry);
                // }
                // });
                //
                // IconWorker { sender, task }
                // });

                // let _ = worker.sender.send_blocking(launcher_entry.clone());

                let bonus_score = history.get(&launcher_entry.path).cloned().unwrap_or(0);

                rows.push(SearchRow {
                    entry: launcher_entry,
                    bonus_score,
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
    let path = if icon.starts_with('/') && std::fs::exists(icon).unwrap_or(false) {
        icon.clone()
    } else {
        let icon = icon.to_string();
        let icon = ICONS.find_icon(icon.as_str(), 32, 1, "Adwaita"); // TODO: find user icon theme

        if let Some(icon) = icon {
            let path = icon.path.to_string_lossy().to_string();

            path
        } else {
            return;
        }
    };

    if let Ok(image) = slint::Image::load_from_path(path.as_str().as_ref()) {
        let buffer = image.to_rgba8().unwrap(); // TODO: unwrap?

        let _ = launcher_entry.icon_resolved.set(buffer);
    }
}

pub struct Launcher {
    search: FuzzySearch<1, SearchRow>,
    bias: LauncherEntryBiasState,
    search_task: smol::Task<std::convert::Infallible>,
    main_window: HideOnDrop<ui::MainWindow>,
    model: Rc<VecModel<ui::SearchRow>>,
    sender: AppSender<Message>,
}

#[derive(Debug, Clone)]
struct LauncherEntry {
    name: String,
    path: PathBuf,
    exec: String,
    icon: Option<String>,
    icon_resolved: OnceLock<slint::SharedPixelBuffer<Rgba8Pixel>>,
}

impl App for Launcher {
    type Message = Message;
    type Output = anyhow::Result<()>;

    const NAME: AppName = AppName::Launcher;

    fn create(message_sender: AppSender<Self::Message>) -> Self {
        // read the bias from persistent state, if any.
        let bias: LauncherEntryBiasState = crate::persistence::read_state("launcher", "entry_bias")
            .ok()
            .unwrap_or_default();

        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let search = FuzzySearch::create_with_config(config);
        let pusher = search.pusher();

        let entries = copy_desktop_entry_cache();

        {
            // TODO: avoid clone, bias should go through FuzzySearch instead
            let bias = bias.history.clone();
            let _ = std::thread::spawn(move || scour_desktop_entries(pusher, &bias));
            // let _ = std::thread::spawn(move || );
        }

        let notify = search.notify();

        let task = {
            let message_sender = message_sender.clone();

            smol::spawn(async move {
                loop {
                    notify.acquire().await;

                    message_sender.send(Message::Search);
                }
            })
        };

        let main_window: HideOnDrop<ui::MainWindow> = ui::MainWindow::new().unwrap().hide_on_drop();

        let model = vec![];

        let model = Rc::new(VecModel::from(model));
        let model_rc: ModelRc<_> = model.clone().into();

        let launcher_entries = main_window.global::<ui::LauncherEntries>();
        launcher_entries.set_entries(model_rc.clone());

        {
            let launcher_search = main_window.global::<ui::LauncherSearch>();
            let message_sender = message_sender.clone();
            launcher_search.on_search_edited(move |query| {
                message_sender.send(Message::QuerySet(query.as_str().to_string()));
            });
        }

        {
            let message_sender = message_sender.clone();
            main_window.on_escape_pressed(move || {
                message_sender.finish();
            });
        }

        {
            let message_sender = message_sender.clone();
            main_window.on_launch(move |index| {
                if index < 0 {
                    return;
                }

                message_sender.send(Message::Launch(index as usize))
            });
        }

        main_window.show().unwrap();

        Launcher {
            // desktop_entries,
            main_window,
            search,
            model,
            bias,
            search_task: task,
            sender: message_sender
        }
    }

    fn on_message(&mut self, message: Self::Message) {
        match message {
            Message::Search => {
                self.search.tick();
                let vec = self
                    .search
                    .get_matches()
                    .into_iter()
                    .cloned()
                    .collect::<Vec<_>>();

                // TODO
                let vec = vec
                    .into_iter()
                    .map(|x| ui::SearchRow {
                        icon: x
                            .entry
                            .icon_resolved
                            .get()
                            .map(|buffer| slint::Image::from_rgba8(buffer.clone()))
                            .unwrap_or_default(),
                        name: x.entry.name.clone().into(),
                    })
                    .collect::<Vec<_>>();

                self.model.set_vec(vec);
            }
            Message::QuerySet(query) => {
                self.search.search::<0>(query);
            }
            Message::Launch(index) => {
                if let Some(search_row) = self.search.get_matches().get(index) {
                    let arc = &search_row.entry;
                    let result = launch(arc.as_ref());
                    self.sender.finish();
                }
            }
        }
    }

    fn stop(self) -> Self::Output {
        Ok(())
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
    QuerySet(String),
    Search,
    Launch(usize),
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

    // fn icon(&self) -> Option<&str> {
    //     self.entry.icon_resolved.get().map(|s| s.as_str())
    // }

    fn path(&self) -> &Path {
        &self.entry.path
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test() {}
}
