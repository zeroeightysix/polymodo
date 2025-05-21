use clap::{Command, CommandFactory, ValueEnum};
use clap_complete::shells;
use std::io::Error;
use std::path::Path;
use shells::Shell;

include!("../src/cli.rs");

fn main() -> Result<(), Error> {
    let outdir = match std::env::var_os("OUT_DIR") {
        None => return Ok(()),
        Some(outdir) => outdir,
    };

    let outdir: &Path = outdir.as_ref();
    let outdir = outdir.join("../../../").canonicalize().unwrap();

    let mut cmd: Command = Args::command();
    
    // shell completions:
    for &shell in Shell::value_variants() {
        let path = clap_complete::generate_to(shell, &mut cmd, "polymodo", &outdir)?;
        println!("cargo:warning=completion file for {shell} is generated: {path:?}");
    }
    
    // manpage:
    let man = clap_mangen::Man::new(cmd);
    let mut buf: Vec<u8> = vec![];
    man.render(&mut buf)?;

    let man_file = outdir.join("polymodo.1");
    std::fs::write(&man_file, buf)?;
    println!("cargo:warning=manpage is generated: {man_file:?}");

    Ok(())
}
