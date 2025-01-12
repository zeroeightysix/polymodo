use anyhow::Context;
use ini::Ini;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// An XDG desktop entry.
#[derive(Debug, Clone)]
pub struct DesktopEntry {
    /// The original path at which this desktop entry is located.
    source_path: PathBuf,
    /// The hash of the desktop entry's content
    source_hash: u64,

    entry_type: ApplicationType,
    name: String,
    generic_name: Option<String>,
    comment: Option<String>,
}

#[derive(Copy, Clone, Debug, strum::EnumString)]
enum ApplicationType {
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
    let main_section = ini.section(Some("Desktop Entry"))
        .context("desktop entry does not have a Desktop Entry section")?;

    let entry_type = main_section.get("Type").context("desktop entry does not have a Type section")?
        .try_into()?;
    let name = main_section.get("Name").context("desktop entry does not have a Name section")?;
    let generic_name = main_section.get("GenericName");
    let comment = main_section.get("Comment");

    Ok(DesktopEntry {
        source_path: path.to_path_buf(),
        source_hash: hash,
        entry_type,
        name: name.to_string(),
        generic_name: generic_name.map(|s| s.to_string()),
        comment: comment.map(|s| s.to_string()),
    })
}

pub fn find_desktop_entries() -> Vec<DesktopEntry> {
    let dirs = xdg::BaseDirectories::new().expect("cannot get base directories");
    let desktop_files = dirs.get_data_dirs().iter()
        .map(|dir| dir.join("applications"))
        .map(|dir| dir.read_dir())
        .filter_map(Result::ok)
        .flat_map(|d| {
            d.filter_map(Result::ok)
                .map(|entry| load(entry.path()))
                .filter_map(Result::ok)
        }).collect::<Vec<_>>();

    desktop_files   
}
