fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = kgui_core::cli::dispatch(&args).unwrap_or_else(|| {
        eprint!("unknown command\n\n");
        let _ = kgui_core::cli::dispatch(&["kgui".into(), "help".into()]);
        2
    });
    std::process::exit(code);
}
