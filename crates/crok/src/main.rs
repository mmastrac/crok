fn main() {
    if let Err(error) = crok::main() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
