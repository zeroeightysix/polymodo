use super::entry::*;
use crate::app::{App, AppExt, AppName, AppSender, JsonAppResult};
use crate::fuzzy_search::FuzzySearch;
use crate::mode::launch::history::LaunchHistory;
use crate::mode::{HideOnDrop, HideOnDropExt};
use crate::ui;
use crate::ui::index_model::IndexModel;
use anyhow::anyhow;
use slint::{ComponentHandle, ModelExt, ModelRc, SharedString};
use std::cmp::Ordering;
use std::io::Write;
use std::os::unix::prelude::CommandExt;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

pub(super) type LauncherEntriesModel = Rc<IndexModel<EntryId, LauncherEntry>>;

#[derive(Debug, Clone)]
pub enum Message {
    QuerySet(String),
    Launch(EntryId),
    NewEntry(EntryId, Arc<DesktopEntry>),
    UpdateIcon(EntryId, Pixels),
    SearchUpdated,
}

pub struct Launcher {
    entries: LauncherEntriesModel,
    #[expect(unused)]
    main_window: HideOnDrop<ui::LauncherWindow>,
    sender: AppSender<Message>,
    search: FuzzySearch<1, SearchEntry>,
    bias: super::LaunchHistory,
}

impl App for Launcher {
    type Message = Message;
    type Output = JsonAppResult<()>;

    const NAME: AppName = AppName::Launcher;

    fn create(message_sender: AppSender<Self::Message>) -> Self {
        // read the bias from persistent state, if any.
        let bias = Self::read_state::<LaunchHistory>().ok().unwrap_or_default();

        let main_window: HideOnDrop<ui::LauncherWindow> =
            ui::LauncherWindow::new().unwrap().hide_on_drop();

        let model: LauncherEntriesModel = Default::default();

        {
            let bias = bias.clone();

            // The model passed to the UI is filtered on the `shown` property on LauncherEntryUi,
            // converted to the slint struct that represents each entry.
            let model = model
                .clone()
                .filter(|entry| entry.shown)
                .sort_by(move |a, b| {
                    let a_bias = bias.score(a.desktop.path.as_path());
                    let b_bias = bias.score(b.desktop.path.as_path());

                    (a_bias, a.score)
                        .partial_cmp(&(b_bias, b.score))
                        .unwrap_or(Ordering::Equal)
                    // .reverse()
                })
                .reverse()
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

        {
            let message_sender = message_sender.clone();
            let _ = std::thread::spawn(move || scour_desktop_entries(message_sender));
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
            bias,
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
                    self.bias.increment_and_decay(desktop.path.clone());
                    if let Err(e) = Self::write_state(&self.bias) {
                        log::error!("couldn't write launcher bias (scoring): {e}");
                    }

                    if let Err(e) = launch(desktop.as_ref()) {
                        log::error!("failed to launch: {e}")
                    }
                    self.sender.finish();
                }
            }
            Message::NewEntry(id, entry) => {
                self.search.push(SearchEntry {
                    for_id: id,
                    text: entry.name.clone(),
                });
                self.entries
                    .insert(id, self.launcher_entry_for_desktop(id, entry));
            }
            Message::UpdateIcon(id, icon) => {
                self.entries.mutate_by_key(&id, |_, _, v| {
                    v.icon = Some(icon);
                });
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
                    let position = matches
                        .iter()
                        .position(|x| x == entry_id)
                        .map(|pos| matches.len() - pos);
                    v.shown = position.is_some();
                    v.score = position.unwrap_or_default() as u32;
                });
            }
        }
    }

    fn stop(self) -> Self::Output {
        JsonAppResult(())
    }
}

impl Launcher {
    fn launcher_entry_for_desktop(&self, id: EntryId, entry: Arc<DesktopEntry>) -> LauncherEntry {
        // Icon loading is offloaded and cached.
        // if we've already got an icon for this entry, or it has failed before,
        // we don't try again:
        let icon = if let Some(icon_path) = entry.icon.as_deref() {
            if is_icon_cached(icon_path) {
                // great! load_icon won't block:
                load_icon(icon_path)
            } else {
                // no cache hit -> we'll have to offload this, and update it later.
                let icon_path = icon_path.to_string();
                let sender = self.sender.clone();
                let offloaded_task = smol::unblock(move || load_icon(&icon_path));

                drop(slint::spawn_local(async move {
                    let icon = offloaded_task.await;
                    if let Some(icon) = icon {
                        sender.send(Message::UpdateIcon(id, icon));
                    }
                }));

                None
            }
        } else {
            None // no icon_path, no icon.
        };

        LauncherEntry {
            id,
            shown: true,
            score: 0,
            desktop: entry,
            icon,
        }
    }
}

#[derive(Default, Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct EntryId(pub usize);

pub struct SearchEntry {
    for_id: EntryId,
    text: SharedString,
}

impl crate::fuzzy_search::Row<1> for SearchEntry {
    type Output = String;

    fn columns(&self) -> [Self::Output; 1] {
        [self.text.to_string()]
    }
}

#[derive(Debug, Clone)]
pub struct LauncherEntry {
    id: EntryId,
    /// Whether this entry should be shown in the UI
    shown: bool,
    /// The score this entry got from the fuzzy matcher
    score: u32,
    /// The desktop entry this corresponds with
    desktop: Arc<DesktopEntry>,
    /// This entry's rendered icon
    icon: Option<Pixels>,
}

impl LauncherEntry {
    pub fn to_slint(&self) -> ui::LauncherEntry {
        let icon = self
            .icon
            .as_ref()
            .map(|buffer| slint::Image::from_rgba8_premultiplied(buffer.clone()))
            .unwrap_or_default();

        ui::LauncherEntry {
            name: self.desktop.name.clone(),
            generic_name: self.desktop.generic_name.clone().unwrap_or_default(),
            description: self.desktop.description.clone().unwrap_or_default(),
            icon,
            id: self.id.0 as i32,
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
