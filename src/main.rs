//! The screen extension, as its node runs it.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // a macOS capture's frames, as screen runs itself for one (src/sck.rs)
    let ran = match args.get(1).map(String::as_str) {
        Some(screen::sck::SUBCOMMAND) => screen::sck::run(&args[2..]),
        _ => screen::serve::serve(),
    };
    if let Err(e) = ran {
        eprintln!("screen: {e:#}");
        std::process::exit(1);
    }
}
