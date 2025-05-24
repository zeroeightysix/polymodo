// This file is included verbatim in the manpage generation build step,
// so it should be kept as minimal as possible!

#[derive(clap::Parser, Debug)]
#[command(name = "polymodo", version, about, long_about = None)]
/// Multimodal window in the centre of your screen that may do things like launch applications
pub struct Args {
    /// Do not connect to or launch the polymodo daemon
    #[arg(long)]
    pub standalone: bool,
    /// If an application of the same type is already running, don't launch it.
    /// This argument does nothing when combined with --standalone, as a standalone instance can't have any apps running already.
    #[arg(long, short)]
    pub single: bool,
}
