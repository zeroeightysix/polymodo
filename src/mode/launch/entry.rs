use super::*;
use crate::fuzzy_search::Row;
use slint::Rgba8Pixel;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;

static DESKTOP_ENTRIES: Mutex<Vec<Arc<LauncherEntry>>> = Mutex::new(Vec::new());

static ICONS: LazyLock<icon::Icons> = LazyLock::new(icon::Icons::new);

#[derive(Debug, Clone)]
pub enum LauncherEntry {
    Desktop(DesktopEntry),
}

impl Row<1> for Arc<LauncherEntry> {
    type Output = nucleo::Utf32String;

    fn columns(&self) -> [Self::Output; 1] {
        [self.name().into()]
    }
}

impl LauncherEntry {
    pub fn name(&self) -> &str {
        let LauncherEntry::Desktop(DesktopEntry { name, .. }) = self;
        name.as_str()
    }

    pub fn path(&self) -> &std::path::Path {
        let LauncherEntry::Desktop(DesktopEntry { path, .. }) = self;
        path.as_path()
    }

    pub fn icon(&self) -> Option<&str> {
        let LauncherEntry::Desktop(DesktopEntry { icon, .. }) = self;
        icon.as_deref()
    }
}

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub name: String,
    pub path: PathBuf,
    pub exec: String,
    pub icon: Option<String>,
    pub icon_resolved: OnceLock<slint::SharedPixelBuffer<Rgba8Pixel>>,
}

struct IconWorker {
    sender: smol::channel::Sender<Arc<LauncherEntry>>,
}

pub fn scour_desktop_entries(pusher: impl Fn(Arc<LauncherEntry>), history: &LaunchHistory) {
    // immediately push cached entries
    {
        let rows = DESKTOP_ENTRIES.lock().unwrap();
        for row in &*rows {
            pusher(row.clone())
        }
    }

    // then start a search for new ones
    let start = Instant::now();
    let entries = crate::xdg::find_desktop_entries();
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
                let launcher_entry = Arc::new(LauncherEntry::Desktop(DesktopEntry {
                    name: entry.name,
                    path: entry.source_path,
                    exec,
                    icon: entry.icon,
                    icon_resolved: OnceLock::new(),
                }));

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

                // let bonus_score = history.get(&launcher_entry.path).cloned().unwrap_or(0);

                rows.push(launcher_entry);

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

    let Some(icon) = launcher_entry.icon() else {
        return;
    };

    // if `Icon` is an absolute path, the image pointed at should be loaded:
    let path = if icon.starts_with('/') && std::fs::exists(icon).unwrap_or(false) {
        icon.to_string()
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

        let LauncherEntry::Desktop(DesktopEntry { icon_resolved, .. }) = launcher_entry.as_ref();
        let _ = icon_resolved.set(buffer);
    }
}
