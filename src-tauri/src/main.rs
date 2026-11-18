// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("hitr {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        if let Err(error) = hitr_lib::mcp::stdio_bridge() {
            eprintln!("{error}");
            std::process::exit(1);
        }
        return;
    }
    hitr_lib::run()
}
