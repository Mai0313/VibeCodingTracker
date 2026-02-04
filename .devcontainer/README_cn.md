# Python 專案 Dev Container 中文說明

本目錄包含 VS Code Dev Container 的設定檔，讓你能在一致的開發環境中進行專案開發。

## 內容說明

- **Dockerfile**：以 Python 3.10 + Node.js 20 為基礎，安裝 zsh、oh-my-zsh、powerlevel10k 及常用插件/字型，提供現代化終端體驗。
- **devcontainer.json**：VS Code 容器設定，包括：
    - 推薦安裝的擴充套件（Python、Jupyter、Docker、TOML、YAML、Git 等）。
    - 掛載本機 `.gitconfig`、`.ssh`、`.p10k.zsh`。
    - 啟動時自動執行 `uv sync && uv cache clean`。

## 使用方式

1. **用 VS Code 開啟本資料夾**（需安裝 [Dev Containers 擴充套件](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers)）。
2. 出現提示時選擇「在容器中重新開啟」，或用命令面板執行 `Dev Containers: Reopen in Container`。
3. 環境會自動建置並安裝所有依賴。

## 自訂化

- **安裝額外 Python/Node 套件**：編輯 Dockerfile。
- **更換 Python 版本**：調整 Dockerfile 的 `PYTHON_VERSION` 參數。
- **新增 VS Code 擴充套件**：編輯 `devcontainer.json` 的 `extensions`。
- **掛載更多檔案**：編輯 `devcontainer.json` 的 `mounts`。

## 常用指令

- **重建容器**：命令面板執行 `Dev Containers: Rebuild Container`。
- **更新依賴**：容器啟動時會自動執行 `uv sync && uv cache clean`。

## 疑難排解

- 若遇到 SSH 或 Git 問題，請確認本機檔案已正確掛載。
- 更多資訊請參考 [VS Code Dev Containers 官方文件](https://code.visualstudio.com/docs/devcontainers/containers)。
