# Installation

## Prerequisites

Before installing *crok*, make sure you have Rust and Cargo installed on your
system. You can install them by following the instructions on the [Rust
installation page](https://www.rust-lang.org/tools/install).

## Installing *crok*

The easiest way to install crok is using Cargo:

```bash
cargo install crok
```

This will download and compile crok, making it available in your system's PATH.

### Faster Install with cargo-binstall

If you have [cargo-binstall](https://github.com/cargo-bins/cargo-binstall)
installed, you can download a pre-built binary instead of compiling from source:

```bash
cargo binstall crok
```

### Installing in CI

For GitHub Actions, the
[cargo-install](https://github.com/baptiste0928/cargo-install) action provides
caching out of the box:

```yaml
- name: Install crok
  uses: baptiste0928/cargo-install@v3
  with:
    crate: crok
```

## Verifying the Installation

After installation, you can verify that *crok* is properly installed by running:

```bash
crok --version
```

This should display the version number of your *crok* installation.

## Updating *crok*

To update crok to the latest version, simply run the installation command again:

```bash
cargo install crok
```

## Uninstalling crok

If you need to uninstall *crok*, you can use Cargo's uninstall command:

```bash
cargo uninstall crok
``` 
