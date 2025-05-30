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

pub fn find_desktop_entries() -> impl Iterator<Item = DesktopEntry> {
    let base_dirs = xdg::BaseDirectories::new();

    base_dirs.data_home
        .into_iter()
        .chain(base_dirs.data_dirs.into_iter())
        .map(|dir| dir.join("applications"))
        .map(|dir| dir.read_dir())
        .filter_map(Result::ok)
        .flat_map(|d| {
            d.filter_map(Result::ok)
                .map(|entry| load(entry.path()))
                .filter_map(Result::ok)
        })
}
