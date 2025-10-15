use super::*;
use crate::app::AppSender;
use once_map::OnceMap;
use slint::{Rgba8Pixel, SharedString};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

type IconPath = String;
pub type Pixels = slint::SharedPixelBuffer<Rgba8Pixel>;

static DESKTOP_ENTRIES: Mutex<Vec<Arc<DesktopEntry>>> = Mutex::new(Vec::new());

static ICONS: LazyLock<icon::Icons> = LazyLock::new(icon::Icons::new);

// contains a None entry if we tried loading the icon, but failed
static ICONS_RENDERED: LazyLock<OnceMap<IconPath, Box<RenderedIcon>>> =
    LazyLock::new(OnceMap::new);

// This is just Option, but with variants named for their meaning.
enum RenderedIcon {
    Ok(Pixels),
    Failed,
}

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub name: SharedString,
    pub path: PathBuf,
    pub exec: String,
    pub icon: Option<String>,
}

fn next_id() -> EntryId {
    static IDX: AtomicUsize = AtomicUsize::new(0);
    let idx = IDX.fetch_add(1, Ordering::Relaxed);
    EntryId(idx)
}

pub fn scour_desktop_entries(sender: AppSender<Message>, history: &LaunchHistory) {
    // immediately push cached entries
    {
        let rows = DESKTOP_ENTRIES.lock().unwrap();
        for row in &*rows {
            sender.send(Message::NewEntry(next_id(), row.clone()));
        }
    }

    // then start a search for new ones
    let start = Instant::now();
    let entries = crate::xdg::find_desktop_entries();
    // and add any new ones to the searcher
    {
        let mut rows = DESKTOP_ENTRIES.lock().unwrap();
        let mut new_entries = 0u32;

        for entry in entries {
            let Some(exec) = entry.exec else {
                continue;
            };

            // an entry with `NoDisplay=true` does not qualify to be shown in the launcher
            if entry.no_display == Some(true) {
                continue;
            }

            // if, for this desktop entry, there exists no SearchRow yet (with comparison being done on the source path)
            if !rows.iter().any(|row| entry.source_path == row.path) {
                log::trace!("new entry {}", entry.source_path.to_string_lossy(),);
                new_entries += 1;

                // add a new search entry for this desktop entry.
                let desktop_entry = Arc::new(DesktopEntry {
                    name: entry.name.into(),
                    path: entry.source_path,
                    exec,
                    icon: entry.icon,
                });

                // let bonus_score = history.get(&launcher_entry.path).cloned().unwrap_or(0);

                rows.push(desktop_entry);

                // and also add it to the fuzzy searcher
                let entry = rows.last().unwrap().clone();
                sender.send(Message::NewEntry(next_id(), entry));
            }
        }

        if new_entries != 0 {
            let time_it_took = Instant::now() - start;

            log::debug!("Took {time_it_took:?} to find {new_entries} new entries");
        }
    }
}

pub fn is_icon_cached(icon: &str) -> bool {
    ICONS_RENDERED.get(icon).is_some()
}

/// Try loading an icon, given its path. This function blocks on I/O.
pub fn load_icon(icon: &str) -> Option<Pixels> {
    if let Some(cached) = ICONS_RENDERED.get(icon) {
        return match cached {
            RenderedIcon::Ok(pixels) => Some(pixels.clone()),
            RenderedIcon::Failed => None,
        };
    }

    // if `Icon` is an absolute path, the image pointed at should be loaded:
    let path = if icon.starts_with('/') && std::fs::exists(icon).unwrap_or(false) {
        icon.to_string()
    } else {
        let icon_string = icon.to_string();
        let icon = ICONS.find_icon(icon_string.as_str(), 32, 1, "Adwaita"); // TODO: find user icon theme

        if let Some(icon) = icon {
            let path = icon.path.to_string_lossy().to_string();

            path
        } else {
            // insert a failed entry into the cache,
            // so that any successive fetches for this icon immediately fail
            ICONS_RENDERED.insert(icon_string, |_| Box::new(RenderedIcon::Failed));
            return None;
        }
    };

    let icon = icon.to_string();
    if let Ok(image) = slint::Image::load_from_path(path.as_str().as_ref()) {
        let buffer = image.to_rgba8().unwrap(); // TODO: unwrap?

        ICONS_RENDERED.insert(icon, |_| Box::new(RenderedIcon::Ok(buffer.clone())));

        Some(buffer)
    } else {
        ICONS_RENDERED.insert(icon, |_| Box::new(RenderedIcon::Failed));

        None
    }
}
