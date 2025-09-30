# Dev Container for Python Project

This directory contains configuration files for developing this project in a fully reproducible environment using [VS Code Dev Containers](https://code.visualstudio.com/docs/devcontainers/containers).

## What's Included?

- **Dockerfile**: Builds an image based on Python 3.10 + Node.js 20, with zsh, oh-my-zsh, powerlevel10k, and useful plugins/fonts for a modern terminal experience.
- **devcontainer.json**: VS Code configuration for the container, including:
    - Recommended extensions for Python, Jupyter, Docker, TOML, YAML, Git, and more.
    - Custom mounts for your local `.gitconfig`, `.ssh`, and `.p10k.zsh`.
    - Automatic dependency sync with `uv` on container start.

## Usage

1. **Open this folder in VS Code** (with the [Dev Containers extension](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers) installed).
2. **Reopen in Container** when prompted, or use the command palette: `Dev Containers: Reopen in Container`.
3. The environment will be built and all dependencies installed automatically.

## Customization

- **Add Python/Node packages**: Edit the Dockerfile to install system or global packages as needed.
- **Change Python version**: Adjust the `PYTHON_VERSION` build argument in the Dockerfile.
- **Add VS Code extensions**: Update the `extensions` list in `devcontainer.json`.
- **Mount more files**: Add to the `mounts` array in `devcontainer.json`.

## Useful Commands

- **Rebuild container**: Use `Dev Containers: Rebuild Container` from the command palette after changing Dockerfile or devcontainer.json.
- **Update dependencies**: The container runs `uv sync && uv cache clean` on start.

## Troubleshooting

- If you have issues with SSH or Git, ensure your local files are correctly mounted.
- For more info, see the [VS Code Dev Containers documentation](https://code.visualstudio.com/docs/devcontainers/containers).
