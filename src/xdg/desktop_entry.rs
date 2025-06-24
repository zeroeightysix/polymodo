use anyhow::Context;
use ini::Ini;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// An XDG desktop entry.
#[derive(Debug, Clone)]
#[expect(unused)]
pub struct DesktopEntry {
    /// The original path at which this desktop entry is located.
    pub source_path: PathBuf,
    /// The hash of the desktop entry's content
    pub source_hash: u64,
    pub entry_type: ApplicationType,
    pub name: String,
    pub exec: Option<String>,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub icon: Option<String>,
    pub no_display: Option<bool>,
}

#[derive(Copy, Clone, Debug, strum::EnumString)]
pub enum ApplicationType {
    Application,
    Link,
    Directory,
}

impl DesktopEntry {}

pub fn load(path: impl AsRef<Path>) -> anyhow::Result<DesktopEntry> {
    let path = path.as_ref();
    let content = std::fs::read_to_string(path)?;
    let hash = {
        let mut hasher = std::hash::DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    };

    let ini = Ini::load_from_str(&content)?;
    let main_section = ini
        .section(Some("Desktop Entry"))
        .context("desktop entry does not have a Desktop Entry section")?;

    let entry_type = main_section
        .get("Type")
        .context("desktop entry does not have a Type section")?
        .try_into()?;
    let name = main_section
        .get("Name")
        .context("desktop entry does not have a Name section")?;
    let generic_name = main_section.get("GenericName");
    let comment = main_section.get("Comment");
    let exec = main_section.get("Exec");
    let icon = main_section.get("Icon");
    let no_display = main_section.get("NoDisplay").and_then(|s| s.parse().ok());

    Ok(DesktopEntry {
        source_path: path.to_path_buf(),
        source_hash: hash,
        entry_type,
        name: name.to_string(),
        exec: exec.map(|s| s.to_string()),
        generic_name: generic_name.map(|s| s.to_string()),
        comment: comment.map(|s| s.to_string()),
        icon: icon.map(|s| s.to_string()),
        no_display,
    })
}

struct DesktopEntryIdentifier<'a> {
    base_dir: &'a Path,
    entry: walkdir::DirEntry,
}

impl DesktopEntryIdentifier<'_> {
    fn relative_dir(&self) -> Option<&Path> {
        self.entry.path().strip_prefix(self.base_dir).ok()
    }
}

fn find_desktop_entries_in_base_dir(
    base_dir: &Path,
) -> impl Iterator<Item = DesktopEntryIdentifier<'_>> {
    walkdir::WalkDir::new(base_dir)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "desktop")
                .unwrap_or(false)
        })
        .map(|e| DesktopEntryIdentifier { base_dir, entry: e })
}

pub fn find_desktop_entries() -> Vec<DesktopEntry> {
    let base_dirs = xdg::BaseDirectories::new();

    let mut data_dirs = base_dirs.data_dirs;
    if let Some(data_home) = base_dirs.data_home {
        data_dirs.insert(0, data_home);
    }

    for dir in &mut data_dirs {
        dir.push("applications");
    }

    let mut desktop_entries = data_dirs
        .iter()
        .flat_map(|dd| find_desktop_entries_in_base_dir(dd))
        .collect::<Vec<_>>();

    // remove duplicate entries
    desktop_entries.dedup_by_key(|e| e.relative_dir().map(|d| d.to_owned()));

    desktop_entries
        .into_iter()
        .filter_map(|e| load(e.entry.path()).ok())
        .collect::<Vec<_>>()
}
