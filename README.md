# basalt-plugin-cargo-project-model

Basalt plugin: Cargo project model provider.

This plugin exposes Cargo workspace and crate metadata to Basalt through the `project-model:cargo` capability.

## Installation

Download the latest `.wasm` from [Releases](https://github.com/adevcorn/basalt-plugin-cargo-project-model/releases) and place it in `~/.config/basalt/plugins/`.

Or install via the Basalt plugin registry.

## Building from source

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/cargo_project_model.wasm ~/.config/basalt/plugins/
```
