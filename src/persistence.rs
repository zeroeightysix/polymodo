use std::path::PathBuf;

const BINCODE_CONFIG: bincode::config::Configuration = bincode::config::standard();

pub trait StorableState {
    const NAME: &'static str;
}

pub fn get_polymodo_state_home() -> Option<PathBuf> {
    let xdg = xdg::BaseDirectories::new();

    xdg.state_home.map(|st| st.join("polymodo"))
}

fn state_file(app_name: &str, state_name: &str) -> Option<PathBuf> {
    let app_home = get_polymodo_state_home().map(|path| path.join(app_name))?;

    // Ensure that the parent of the state file exists, recursively.
    if !app_home.exists() {
        std::fs::create_dir_all(app_home.as_path()).ok()?;
    }

    let state_file = app_home.join(state_name);

    Some(state_file)
}

pub fn read_state<S: bincode::Decode<()>>(app_name: &str, state_name: &str) -> std::io::Result<S> {
    let file = state_file(app_name, state_name)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;

    let file = std::fs::File::open(file)?;
    let mut buf_read = std::io::BufReader::new(file);

    bincode::decode_from_std_read(&mut buf_read, BINCODE_CONFIG).map_err(std::io::Error::other)
}

pub fn write_state<S: bincode::Encode>(
    app_name: &str,
    state_name: &str,
    state: S,
) -> std::io::Result<usize> {
    let file = state_file(app_name, state_name)
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;

    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(file)?;
    let mut buf_write = std::io::BufWriter::new(file);

    bincode::encode_into_std_write(state, &mut buf_write, BINCODE_CONFIG)
        .map_err(std::io::Error::other)
}
