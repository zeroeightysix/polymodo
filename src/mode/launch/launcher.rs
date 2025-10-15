use super::entry::*;
use crate::app::{App, AppName, AppSender, JsonAppResult};
use crate::fuzzy_search::FuzzySearch;
use crate::mode::{HideOnDrop, HideOnDropExt};
use crate::ui;
use crate::ui::index_model::IndexModel;
use anyhow::anyhow;
use slint::{ComponentHandle, ModelExt, ModelRc};
use std::io::Write;
use std::os::unix::prelude::CommandExt;
use std::process::Command;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

pub(super) type LauncherEntriesModel = Rc<IndexModel<EntryId, LauncherEntry>>;

#[derive(Debug, Clone)]
pub enum Message {
    QuerySet(String),
    Launch(EntryId),
    NewEntry(Arc<DesktopEntry>),
    SearchUpdated,
}

pub struct Launcher {
    entries: LauncherEntriesModel,
    #[expect(unused)]
    main_window: HideOnDrop<ui::LauncherWindow>,
    sender: AppSender<Message>,
    search: FuzzySearch<1, SearchEntry>,
}

impl App for Launcher {
    type Message = Message;
    type Output = JsonAppResult<()>;

    const NAME: AppName = AppName::Launcher;

    fn create(message_sender: AppSender<Self::Message>) -> Self {
        // read the bias from persistent state, if any.
        let bias: super::LauncherEntryBiasState =
            crate::persistence::read_state("launcher", "entry_bias")
                .ok()
                .unwrap_or_default();

        let main_window: HideOnDrop<ui::LauncherWindow> =
            ui::LauncherWindow::new().unwrap().hide_on_drop();

        let model: LauncherEntriesModel = Default::default();

        {
            // The model passed to the UI is filtered on the `shown` property on LauncherEntryUi,
            // converted to the slint struct that represents each entry.
            let model = model
                .clone()
                .filter(|entry| entry.shown)
                .map(|entry| entry.to_slint());

            main_window
                .global::<ui::LauncherEntries>()
                .set_entries(ModelRc::new(model));
        }

        let search: FuzzySearch<1, SearchEntry> = FuzzySearch::create_with_config({
            let mut config = nucleo::Config::DEFAULT;
            config.prefer_prefix = true;
            config
        });

        let pusher = {
            let sender = message_sender.clone();

            move |entry: Arc<DesktopEntry>| sender.send(Message::NewEntry(entry))
        };

        {
            // TODO: avoid clone, bias should go through FuzzySearch instead
            let bias = bias.history.clone();
            let _ = std::thread::spawn(move || scour_desktop_entries(pusher, &bias));
            // let _ = std::thread::spawn(move || );
        }

        {
            let notify = search.notify();
            let sender = message_sender.clone();
            message_sender.spawn(async move {
                loop {
                    notify.acquire().await;

                    sender.send(Message::SearchUpdated)
                }
            });
        }

        // On search query edit
        {
            let message_sender = message_sender.clone();
            main_window
                .global::<ui::LauncherSearch>()
                .on_search_edited(move |query| {
                    message_sender.send(Message::QuerySet(query.as_str().to_string()));
                });
        }

        // On escape
        {
            let message_sender = message_sender.clone();
            main_window.on_escape_pressed(move || {
                message_sender.finish();
            });
        }

        // On enter (launch)
        {
            let message_sender = message_sender.clone();
            main_window.on_launch(move |id| {
                if id < 0 {
                    return;
                }

                message_sender.send(Message::Launch(EntryId(id as usize)))
            });
        }

        main_window.show().unwrap();

        Launcher {
            entries: model,
            search,
            main_window,
            sender: message_sender,
        }
    }

    fn on_message(&mut self, message: Self::Message) {
        match message {
            Message::QuerySet(query) => {
                self.search.search::<0>(query);
            }
            Message::Launch(entry_id) => {
                if let Some(LauncherEntry { desktop, .. }) =
                    self.entries.get_value_of_key(&entry_id)
                {
                    // TODO: handle?
                    let result = launch(desktop.as_ref());
                    self.sender.finish();
                }
            }
            Message::SearchUpdated => {
                self.search.tick();

                let matches: Vec<_> = self
                    .search
                    .get_matches()
                    .into_iter()
                    .map(|entry| entry.for_id)
                    .collect();

                self.entries.mutate_all(|_, entry_id, v| {
                    let shown = matches.contains(entry_id);
                    v.shown = shown;
                });
            }
            Message::NewEntry(entry) => {
                static IDX: AtomicUsize = AtomicUsize::new(0);

                let idx = IDX.fetch_add(1, Ordering::Relaxed);
                let id = EntryId(idx);

                self.search.push(SearchEntry {
                    for_id: id,
                    text: entry.name.clone(),
                });
                self.entries.insert(
                    id,
                    LauncherEntry {
                        id,
                        shown: true,
                        desktop: entry,
                    },
                );
            }
        }
    }

    fn stop(self) -> Self::Output {
        JsonAppResult(())
    }
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct EntryId(pub usize);

pub struct SearchEntry {
    for_id: EntryId,
    text: String,
}

impl crate::fuzzy_search::Row<1> for SearchEntry {
    type Output = nucleo::Utf32String;

    fn columns(&self) -> [Self::Output; 1] {
        [self.text.clone().into()]
    }
}

#[derive(Debug, Clone)]
pub struct LauncherEntry {
    id: EntryId,
    /// Whether this entry should be shown in the UI
    shown: bool,
    /// The desktop entry this corresponds with
    desktop: Arc<DesktopEntry>,
}

impl LauncherEntry {
    pub fn to_slint(&self) -> ui::LauncherEntry {
        let DesktopEntry {
            icon_resolved,
            name,
            ..
        } = self.desktop.as_ref();

        let icon = icon_resolved
            .get()
            .map(|buffer| slint::Image::from_rgba8(buffer.clone()))
            .unwrap_or_default();

        ui::LauncherEntry {
            icon,
            id: self.id.0 as i32,
            name: name.into(),
        }
    }
}

fn launch(desktop: &DesktopEntry) -> anyhow::Result<()> {
    match fork::fork().map_err(|_| anyhow!("failed to fork process"))? {
        fork::Fork::Child => {
            // detach
            if let Err(e) = nix::unistd::daemon(false, false) {
                log::error!("daemonize failed: {}", e);
            }

            // %f and %F: lists of files. polymodo does not yet support selecting files.
            let exec = desktop.exec.replace("%f", "").replace("%F", "");
            // same story for %u and %U:
            let exec = exec.replace("%u", "").replace("%U", "");

            // split exec by spaces
            let mut args = exec
                .split(" ")
                .flat_map(|arg| match arg {
                    "%i" => vec!["--icon", desktop.icon.as_deref().unwrap_or("")],
                    "%c" => vec![desktop.name.as_str()],
                    "%k" => {
                        vec![desktop.path.as_os_str().to_str().unwrap_or("")]
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
            log::info!("Launching {:?} with pid {pid}", desktop.name.as_str());

            let _ = std::io::stdout().flush();
            Ok(())
        }
    }
}
