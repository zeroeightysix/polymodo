use super::entry::*;
use crate::app::{App, AppName, AppSender};
use crate::mode::{HideOnDrop, HideOnDropExt};
use crate::ui;
use anyhow::anyhow;
use slint::{ComponentHandle, Model, ModelExt, ModelRc, VecModel};
use std::io::Write;
use std::os::unix::prelude::CommandExt;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;
use crate::fuzzy_search::FuzzySearch;

pub(super) type LauncherEntriesModel = Rc<VecModel<LauncherEntryUi>>;

#[derive(Debug, Clone)]
pub enum Message {
    QuerySet(String),
    Launch(usize),
    SearchUpdated,
}

pub struct Launcher {
    entries: LauncherEntriesModel,
    filtered_entries: ModelRc<LauncherEntryUi>,
    #[expect(unused)]
    main_window: HideOnDrop<ui::LauncherWindow>,
    sender: AppSender<Message>,
    search: FuzzySearch<1, Arc<LauncherEntry>>,
}

impl App for Launcher {
    type Message = Message;
    type Output = anyhow::Result<()>;

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

        // The model passed to the UI is filtered on the `shown` property on LauncherEntryUi,
        // converted to the slint struct that represents each entry.
        let filtered_model = Rc::new(model.clone().filter(|entry| entry.shown));

        {
            let mapped_model = filtered_model.clone().map(|entry| entry.to_slint());

            main_window
                .global::<ui::LauncherEntries>()
                .set_entries(ModelRc::new(mapped_model));
        }

        let mut config = nucleo::Config::DEFAULT;
        config.prefer_prefix = true;
        let search = FuzzySearch::create_with_config(config);
        let pusher = search.pusher();
        let notify = search.notify();

        {
            // TODO: avoid clone, bias should go through FuzzySearch instead
            let bias = bias.history.clone();
            let _ = std::thread::spawn(move || scour_desktop_entries(pusher, &bias));
            // let _ = std::thread::spawn(move || );
        }

        {
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
            main_window.on_launch(move |index| {
                if index < 0 {
                    return;
                }

                message_sender.send(Message::Launch(index as usize))
            });
        }

        main_window.show().unwrap();

        Launcher {
            entries: model,
            filtered_entries: filtered_model.into(),
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
            Message::Launch(index) => {
                if let Some(LauncherEntryUi { entry, .. }) = self.filtered_entries.row_data(index) {
                    // TODO: handle?
                    let result = launch(entry.as_ref());
                    self.sender.finish();
                }
            }
            Message::SearchUpdated => {
                self.search.tick();
                let matches: Vec<_> = self.search.get_matches()
                    .into_iter()
                    .map(Arc::clone)
                    .map(LauncherEntryUi::from)
                    .collect();
                self.entries.set_vec(matches);
            }
        }
    }

    fn stop(self) -> Self::Output {
        Ok(())
    }
}

impl Launcher {
    pub fn mutate_entry<R>(
        &self,
        row: usize,
        map: impl FnOnce(&mut LauncherEntryUi) -> R,
    ) -> Option<R> {
        if let Some(mut entry_ui) = self.entries.row_data(row) {
            let r = map(&mut entry_ui);

            self.entries.set_row_data(row, entry_ui);

            Some(r)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct LauncherEntryUi {
    /// Whether this entry should be shown in the UI
    shown: bool,
    /// The launcher entry this corresponds with
    entry: Arc<LauncherEntry>,
}

impl LauncherEntryUi {
    pub fn to_slint(&self) -> ui::LauncherEntry {
        let LauncherEntry::Desktop(DesktopEntry {
            icon_resolved,
            name,
            ..
        }) = self.entry.as_ref();

        let icon = icon_resolved
            .get()
            .map(|buffer| slint::Image::from_rgba8(buffer.clone()))
            .unwrap_or_default();

        ui::LauncherEntry {
            icon,
            name: name.into(),
        }
    }
}

impl From<Arc<LauncherEntry>> for LauncherEntryUi {
    fn from(value: Arc<LauncherEntry>) -> Self {
        Self {
            shown: true,
            entry: value,
        }
    }
}

fn launch(entry: &LauncherEntry) -> anyhow::Result<()> {
    let LauncherEntry::Desktop(entry) = entry;

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
