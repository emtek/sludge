FROM registry.fedoraproject.org/fedora-minimal:44

ARG USERNAME=vscode
ARG USER_UID=1000
ARG USER_GID=$USER_UID

# Toolchain installed via rustup so we are not pinned to the distro Rust.
ARG RUST_VERSION=stable

# fedora-minimal ships microdnf; install everything in one layer.
#
# Build/runtime libs:
#   gtk4-devel, libadwaita-devel    -> gtk4 + libadwaita crates
#   pkgconf-pkg-config              -> pkg-config used by gtk-rs build scripts
#   openssl-devel                   -> tokio-tungstenite (native-tls) + openssl-sys
#   gcc, gcc-c++, make              -> C/C++ toolchain (rusqlite bundled, ring, openssl-sys)
#   sqlite-devel                    -> not strictly required (rusqlite is bundled) but handy
#   glib2-devel, gdk-pixbuf2-devel  -> pulled in transitively, listed for clarity
#   gobject-introspection-devel     -> required by some gtk-rs sys crates at build time
#   cairo-gobject-devel, pango-devel, graphene-devel
#                                   -> GTK4 transitive dev headers
# Runtime helpers:
#   hicolor-icon-theme, gtk-update-icon-cache, desktop-file-utils, dbus
#                                   -> needed if you ever run the binary inside the container
# Shell/UX extras:
#   fish, zoxide                    -> default shell + smarter cd
# Misc:
#   git, gh, curl, tar, gzip, ca-certificates, findutils, which, sudo, shadow-utils
RUN microdnf install -y --setopt=install_weak_deps=0 --nodocs \
        gcc gcc-c++ make pkgconf-pkg-config \
        gtk4-devel libadwaita-devel \
        glib2-devel gdk-pixbuf2-devel \
        gobject-introspection-devel \
        cairo-gobject-devel pango-devel graphene-devel \
        openssl-devel sqlite-devel \
        hicolor-icon-theme desktop-file-utils dbus-libs \
        fish zoxide \
        git gh curl tar gzip ca-certificates findutils which sudo shadow-utils \
    && microdnf clean all \
    && rm -rf /var/cache/yum /var/cache/dnf

# Starship prompt — install from upstream (Fedora package lags).
RUN curl -sSf https://starship.rs/install.sh \
        | sh -s -- --yes --bin-dir /usr/local/bin

# Create non-root user with passwordless sudo and fish as the login shell.
RUN groupadd --gid $USER_GID $USERNAME \
    && useradd  --uid $USER_UID --gid $USER_GID -m -s /usr/bin/fish $USERNAME \
    && echo "$USERNAME ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/$USERNAME \
    && chmod 0440 /etc/sudoers.d/$USERNAME

USER $USERNAME
ENV CARGO_HOME=/home/$USERNAME/.cargo \
    RUSTUP_HOME=/home/$USERNAME/.rustup \
    PATH=/home/$USERNAME/.local/bin:/home/$USERNAME/.cargo/bin:$PATH

# Install rustup + the requested toolchain. clippy + rustfmt + rust-analyzer
# come from rustup so editor tooling works out of the box.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain $RUST_VERSION --profile minimal \
            --component clippy --component rustfmt --component rust-analyzer \
    && rustc --version && cargo --version

# Claude Code — native installer (per-user, lands in ~/.local/bin/claude).
RUN curl -fsSL https://claude.ai/install.sh | bash \
    && /home/$USERNAME/.local/bin/claude --version

# Fish config: PATH for local + cargo bin, plus starship + zoxide init.
RUN mkdir -p /home/$USERNAME/.config/fish/conf.d \
    && printf '%s\n' \
        'set -gx PATH $HOME/.local/bin $HOME/.cargo/bin $PATH' \
        'set -gx RUSTUP_HOME $HOME/.rustup' \
        'set -gx CARGO_HOME $HOME/.cargo' \
        'if status is-interactive' \
        '    starship init fish | source' \
        '    zoxide init fish | source' \
        'end' \
        > /home/$USERNAME/.config/fish/config.fish

WORKDIR /workspaces/sludge
