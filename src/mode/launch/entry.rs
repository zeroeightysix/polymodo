use super::*;
use crate::app::AppSender;
use indexmap::IndexMap;
use once_map::OnceMap;
use slint::SharedString;
use smol::io::AsyncReadExt;
use std::cell::LazyCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

type IconPath = String;

static DESKTOP_ENTRIES: Mutex<LazyCell<IndexMap<PathBuf, Arc<DesktopEntry>>>> =
    Mutex::new(LazyCell::new(IndexMap::new));

static ICONS: LazyLock<Mutex<icon::IconsCache>> = LazyLock::new(|| {
    let mut cache: icon::IconsCache = icon::Icons::new().into();
    cache.pre_populate_cache();
    Mutex::new(cache)
});

// contains a None entry if we tried loading the icon, but failed
static ICONS_RENDERED: LazyLock<OnceMap<IconPath, Box<RenderedIcon>>> = LazyLock::new(OnceMap::new);

// This is just Option, but with variants named for their meaning.
enum RenderedIcon {
    Ok(StaticImage),
    Failed,
}

#[derive(Debug, Clone)]
pub struct StaticImage {
    data: &'static [u8],
    extension: String,
}

impl StaticImage {
    pub fn to_slint_image(&self) -> slint::Image {
        let StaticImage { data, extension } = self;

        slint::private_unstable_api::re_exports::load_image_from_embedded_data(
            (*data).into(),
            extension.as_bytes().into(),
        )
    }
}

#[derive(Debug, Clone)]
pub struct DesktopEntry {
    pub name: SharedString,
    pub generic_name: Option<SharedString>,
    pub description: Option<SharedString>,
    pub path: PathBuf,
    pub exec: String,
    pub icon: Option<String>,
}

fn next_id() -> EntryId {
    static IDX: AtomicUsize = AtomicUsize::new(0);
    let idx = IDX.fetch_add(1, Ordering::Relaxed);
    EntryId(idx)
}

pub fn scour_desktop_entries(sender: AppSender<Message>) {
    // immediately push cached entries
    {
        let rows = DESKTOP_ENTRIES.lock().unwrap();
        for (_, row) in rows.iter() {
            sender.send(Message::NewEntry(next_id(), row.clone()));
        }
    }

    // then start a search for new ones
    let start = Instant::now();
    let entries: Vec<_> = crate::xdg::find_desktop_entries();

    // and add any new ones to the searcher
    {
        let mut cache = DESKTOP_ENTRIES.lock().unwrap();
        let mut new_entries = 0u32;
        let mut new_cache = vec![];

        for entry in entries {
            // remove entries that aren't fit for being shown in the launcher
            let Some(exec) = entry.exec else {
                continue;
            };

            // an entry with `NoDisplay=true` does not qualify to be shown in the launcher
            if entry.no_display == Some(true) {
                continue;
            }

            // Does this entry exist in the cache already?
            let entry = match cache.get(&entry.source_path) {
                Some(de) => de.clone(),
                // if not, make one!
                None => {
                    log::trace!("new entry {}", entry.source_path.to_string_lossy());
                    new_entries += 1;

                    // add a new search entry for this desktop entry.
                    let desktop_entry = Arc::new(DesktopEntry {
                        name: entry.name.into(),
                        generic_name: entry.generic_name.clone().map(Into::into),
                        description: entry.comment.clone().map(Into::into),
                        path: entry.source_path,
                        exec,
                        icon: entry.icon,
                    });

                    // and also add it to the fuzzy searcher
                    sender.send(Message::NewEntry(next_id(), desktop_entry.clone()));

                    desktop_entry
                }
            };

            new_cache.push(entry);
        }

        **cache = new_cache
            .into_iter()
            .map(|d| (d.path.clone(), d)) // map each desktop entry to its path
            .collect();

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
/// This function utilizes a cache to avoid reading the same icon twice, but the cache
/// persists forever: all icons are leaked (to obtain a 'static reference) and placed into the cache.
pub async fn load_icon(icon: &str) -> Option<StaticImage> {
    if let Some(cached) = ICONS_RENDERED.get(icon) {
        return match cached {
            RenderedIcon::Ok(static_image) => Some(static_image.clone()),
            RenderedIcon::Failed => None,
        };
    }

    // if `Icon` is an absolute path, the image pointed at should be loaded:
    let path = if icon.starts_with('/') && std::fs::exists(icon).unwrap_or(false) {
        icon.to_string()
    } else {
        let icon_string = icon.to_string();
        let icon = ICONS.lock().unwrap().find_default_icon(icon_string.as_str(), 32, 1);

        if let Some(icon) = icon {
            icon.path().to_string_lossy().to_string()
        } else {
            // insert a failed entry into the cache,
            // so that any successive fetches for this icon immediately fail
            ICONS_RENDERED.insert(icon_string, |_| Box::new(RenderedIcon::Failed));
            return None;
        }
    };

    async fn read(path: &std::path::Path) -> Option<&'static [u8]> {
        use smol::fs::*;

        let mut file = File::open(path).await.ok()?;
        let mut bytes = vec![];
        let bytes_read = file.read_to_end(&mut bytes).await.ok()?;

        if bytes_read == 0 {
            return None;
        }

        let bytes = bytes.leak(); // fight me
        Some(bytes)
    }

    let path = path.as_str().as_ref();
    let image_data = read(path).await;
    let extension = path.extension().map(|ext| ext.to_string_lossy());
    let extension = extension.as_deref().unwrap_or_default().to_string();

    let icon = icon.to_string();
    if let Some(image_data) = image_data {
        let static_image = StaticImage {
            data: image_data,
            extension,
        };

        ICONS_RENDERED.insert(icon, |_| Box::new(RenderedIcon::Ok(static_image.clone())));

        Some(static_image)
    } else {
        ICONS_RENDERED.insert(icon, |_| Box::new(RenderedIcon::Failed));

        None
    }
}
