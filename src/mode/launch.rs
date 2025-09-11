use crate::fuzzy_search::{FuzzySearch, Row};
use crate::windowing::app::{App, AppName, AppSender};
use crate::xdg::find_desktop_entries;
use anyhow::anyhow;
use icon::Icons;
use nucleo::Utf32String;
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;
use slint::{ComponentHandle, ModelRc, VecModel};
use crate::modules::{MainWindow, TestModelItem};

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
    sender: smol::channel::Sender<Arc<LauncherEntry>>,
    task: smol::Task<Option<()>>,
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

        // TODO: dropping this will cancel the work task
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
                    let (sender, receiver) = smol::channel::unbounded();
                    let task = smol::unblock(move || -> Option<()> {
                        loop {
                            let entry = receiver.recv_blocking().ok()?;

                            find_and_set_icon(&entry);
                        }
                    });

                    IconWorker { sender, task }
                });

                let _ = worker.sender.send(launcher_entry.clone());

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
    search: FuzzySearch<1, SearchRow>,
    results: Vec<SearchRow>,
    bias: LauncherEntryBiasState,
    search_task: smol::Task<std::convert::Infallible>,
}

#[derive(Debug, Clone)]
struct LauncherEntry {
    name: String,
    path: PathBuf,
    exec: String,
    icon: Option<String>,
    icon_resolved: OnceLock<String>,
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

        // {
        //     // TODO: avoid clone, bias should go through FuzzySearch instead
        //     let bias = bias.history.clone();
        //     tokio::task::spawn_blocking(move || scour_desktop_entries(pusher, &bias));
        // }

        let notify = search.notify();

        let task = smol::spawn(async move {
            loop {
                notify.acquire().await;

                message_sender.send(Message::Search);
            }
        });

        let main_window: MainWindow = MainWindow::new().expect("dkjfl;sdjfs");

        let model = vec![
            TestModelItem { name: "Foo".into() },
            TestModelItem { name: "Bar".into() },
            TestModelItem { name: "Baz".into() },
        ];

        let model = Rc::new(VecModel::from(model));
        let model_rc: ModelRc<_> = model.clone().into();

        main_window.set_texts(model_rc.clone());
        main_window.on_btn_clicked(move || model.push(TestModelItem { name: "Bar".into() }));
        main_window.show();

        Launcher {
            // desktop_entries,
            search,
            results: entries,
            bias,
            search_task: task,
        }
    }

    fn on_message(&mut self, message: Self::Message) {
        match message {
            Message::Search => {
                self.search.tick();
                self.results = self.search.get_matches().into_iter().cloned().collect();
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
    fn test() {}
}
