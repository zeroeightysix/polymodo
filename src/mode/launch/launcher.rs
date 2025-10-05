use super::entry::*;
use crate::app::{App, AppName, AppSender};
use crate::fuzzy_search::{FuzzySearch, Row};
use crate::mode::{HideOnDrop, HideOnDropExt};
use crate::ui;
use anyhow::anyhow;
use slint::{ComponentHandle, ModelRc, Rgba8Pixel, VecModel};
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub enum Message {
    QuerySet(String),
    UpdateSearchResults,
    Launch(usize),
}

pub struct Launcher {
    search: FuzzySearch<1, SearchRow>,
    bias: super::LauncherEntryBiasState,
    search_task: smol::Task<std::convert::Infallible>,
    main_window: HideOnDrop<ui::LauncherWindow>,
    model: Rc<VecModel<ui::SearchRow>>,
    sender: AppSender<Message>,
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

                    message_sender.send(Message::UpdateSearchResults);
                }
            })
        };

        let main_window: HideOnDrop<ui::LauncherWindow> =
            ui::LauncherWindow::new().unwrap().hide_on_drop();

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
            sender: message_sender,
        }
    }

    fn on_message(&mut self, message: Self::Message) {
        match message {
            Message::UpdateSearchResults => {
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
