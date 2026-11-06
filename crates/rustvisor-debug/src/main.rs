//! `rvdbg`: rustvisor-specific debugging front end. Only `--version` for now.
#![forbid(unsafe_code)]

fn main() {
    let mut args = std::env::args().skip(1);
    match (args.next().as_deref(), args.next()) {
        (Some("--version" | "-V"), None) => {
            println!("rvdbg {}", env!("CARGO_PKG_VERSION"));
        }
        _ => {
            eprintln!("rvdbg: not implemented yet; only --version is available");
            std::process::exit(2);
        }
    }
}
