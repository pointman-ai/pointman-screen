//! The screen extension, as its node runs it.

fn main() {
    if let Err(e) = screen::serve::serve() {
        eprintln!("screen: {e:#}");
        std::process::exit(1);
    }
}
