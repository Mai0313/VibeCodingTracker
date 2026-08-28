# Dev Container for Rust Project

This directory contains configuration for developing this project in a reproducible environment using [VS Code Dev Containers](https://code.visualstudio.com/docs/devcontainers/containers).

## What's Included?

There is no Dockerfile. The environment is assembled from a stock image plus [Dev Container Features](https://containers.dev/features), which keeps the image small and removes the base-image tag maintenance a hand-written Dockerfile carries.

- **Base image**: `mcr.microsoft.com/devcontainers/base:bookworm`.
- **Features**:
    - `common-utils`: git, curl, zsh, oh-my-zsh, and a non-root `vscode` user with passwordless sudo. zsh is the default shell.
    - `rust`: the Rust toolchain with `rust-analyzer`, `rust-src`, `rustfmt` and `clippy`, plus the C toolchain Cargo needs to link.
    - `uv`: used by the Python CLI wrapper under `cli/python/` and by `pre-commit`.
- **devcontainer.json**: extension recommendations (rust-analyzer, even-better-toml, Docker, GitLens, YAML) and a zsh terminal profile.
- **updateContentCommand**: runs `cargo fetch`, so dependencies are downloaded when the container opens.

If a crate you add needs extra native build dependencies, add `cmake`, `clang` or `lld` through a feature or `postCreateCommand` rather than reintroducing a Dockerfile for them.

## Git and SSH

Nothing is mounted for git or SSH, and nothing needs to be. VS Code Dev Containers copies your local `.gitconfig` into the container and forwards your local SSH agent automatically, so `git push` over SSH works with your private keys never leaving the host. GitHub Codespaces does the equivalent.

## Personal shell setup

The container ships a plain oh-my-zsh. Prompt themes, plugins and aliases are personal preference rather than project configuration, so they are deliberately not baked in here.

To get your own setup in every container you open, set `dotfiles.repository` in your VS Code settings once, or enable **Automatically install dotfiles** under [GitHub Settings > Codespaces](https://github.com/settings/codespaces). Either one applies to every container you create and affects nobody else working on this repository.

## Usage

1. Open this folder in VS Code with the [Dev Containers extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers) installed.
2. **Reopen in Container** when prompted, or run `Dev Containers: Reopen in Container` from the command palette.
3. Build with `make build` or `cargo build` as usual.

## Customization

- **Pin a Rust version**: pass `{"version": "1.XX"}` to the `rust` feature. `rust-toolchain.toml` still governs what Cargo uses inside the project.
- **Add a tool**: prefer a feature from [containers.dev/features](https://containers.dev/features) over reintroducing a Dockerfile.
- **Add VS Code extensions**: update the `extensions` list in `devcontainer.json`.

## Troubleshooting

- **After editing `devcontainer.json`**: run `Dev Containers: Rebuild Container`.
- **SSH keys not working**: check that a local ssh-agent is running and that `ssh-add -l` lists your key. The container holds no keys of its own.
- **rust-analyzer slow on first open**: the initial index and dependency download take a while.
- **Permission issues**: the container runs as `vscode`; check file ownership if a write fails.
- For more, see the [VS Code Dev Containers documentation](https://code.visualstudio.com/docs/devcontainers/containers).
