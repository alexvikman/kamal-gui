fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(kgui_core::cli::main(&args));
}
