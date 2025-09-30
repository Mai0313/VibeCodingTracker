FROM mcr.microsoft.com/devcontainers/rust:1-1-bookworm

LABEL maintainer="Wei Lee <mai@mai0313.com>" \
      org.label-schema.name="codex_usage" \
      org.label-schema.vendor="Wei Lee" \
      org.label-schema.schema-version="1.0" \
      com.centurylinklabs.watchtower.stop-signal="SIGINT"

# Optional: Git SSL verify disabled to match workflows (keep per request)
RUN git config --global http.sslVerify false

# Install common build tooling for Rust native deps
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config cmake clang lld && \
    rm -rf /var/lib/apt/lists/*

# Install Fonts for zsh p10k
# 1. 安裝 zsh vim wget 和 fontconfig
RUN apt-get update && apt-get install -y zsh vim wget curl fontconfig && \
    mkdir -p "/usr/local/share/fonts/meslo" && \
    wget -O "/usr/local/share/fonts/meslo/MesloLGS NF Regular.ttf" "https://github.com/romkatv/dotfiles-public/raw/master/.local/share/fonts/NerdFonts/MesloLGS%20NF%20Regular.ttf" && \
    wget -O "/usr/local/share/fonts/meslo/MesloLGS NF Bold.ttf" "https://github.com/romkatv/dotfiles-public/raw/master/.local/share/fonts/NerdFonts/MesloLGS%20NF%20Bold.ttf" && \
    wget -O "/usr/local/share/fonts/meslo/MesloLGS NF Italic.ttf" "https://github.com/romkatv/dotfiles-public/raw/master/.local/share/fonts/NerdFonts/MesloLGS%20NF%20Italic.ttf" && \
    wget -O "/usr/local/share/fonts/meslo/MesloLGS NF Bold Italic.ttf" "https://github.com/romkatv/dotfiles-public/raw/master/.local/share/fonts/NerdFonts/MesloLGS%20NF%20Bold%20Italic.ttf" && \
    fc-cache -fv

# 安裝 oh-my-zsh（非互動）
RUN bash -lc 'yes | sh -c "$(curl -fsSL https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh)" || true'

# 安裝 powerlevel10k theme
RUN git clone --depth=1 https://github.com/romkatv/powerlevel10k.git ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/themes/powerlevel10k

# 安裝 plugins
RUN git clone --depth=1 https://github.com/zsh-users/zsh-autosuggestions ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-autosuggestions && \
    git clone --depth=1 https://github.com/zsh-users/zsh-syntax-highlighting.git ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-syntax-highlighting && \
    git clone --depth=1 https://github.com/agkozak/zsh-z ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-z

# 修改 .zshrc 設定 theme 與 plugins
RUN sed -i 's|^ZSH_THEME=.*$|ZSH_THEME="powerlevel10k/powerlevel10k"|' $HOME/.zshrc && \
    sed -i 's|^plugins=(.*)$|plugins=(git zsh-autosuggestions zsh-syntax-highlighting zsh-z)|' $HOME/.zshrc && \
    sed -i 's|^# ZSH_CUSTOM=.*$|ZSH_CUSTOM=$HOME/.oh-my-zsh/custom|' $HOME/.zshrc && \
    sed -i 's|^# export PATH=\$HOME/bin:\$HOME/.local/bin:/usr/local/bin:\$PATH$|export PATH=$HOME/bin:$HOME/.local/bin:/usr/local/bin:$PATH|' $HOME/.zshrc

# 設定 zsh 為預設 shell
RUN chsh -s $(which zsh)
