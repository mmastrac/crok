fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!(
        "warning: `clitest` has been renamed to `crok`! Use `crok` instead (cargo install crok)"
    );

    crok::main()
}
