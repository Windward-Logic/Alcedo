# Alcedo

Native GTK4/libadwaita IMAP/SMTP mail client for GNOME (Ubuntu 26.04 / GNOME 50).

## Install (Ubuntu 26.04)

From a source checkout:

```bash
./scripts/install-ubuntu-26.sh
```

This installs GTK4/libadwaita/WebKitGTK build packages, `rustc`/`cargo` from Ubuntu (1.93), builds a release binary, and copies it plus the desktop entry, icon, and GSettings schema to `$HOME/.local`. Use `--prefix /usr/local` for a system install, or `--deps-only` to only install packages.

## Build

Install system dependencies (also done by `scripts/install-ubuntu-26.sh`):

```bash
sudo apt install -y \
  build-essential \
  clang \
  libclang-dev \
  pkg-config \
  cmake \
  nasm \
  rustc \
  cargo \
  libgtk-4-dev \
  libadwaita-1-dev \
  libwebkitgtk-6.0-dev \
  libspelling-1-dev \
  libenchant-2-dev \
  libgtksourceview-5-dev \
  libglib2.0-dev \
  librsvg2-dev \
  nettle-dev \
  desktop-file-utils
```

Install a runtime spell-check dictionary for your locale (for example `hunspell-en-us`).

```bash
cargo check --workspace
cargo test -p alcedo-core
```

If `nettle-sys` fails with `stddef.h` not found during a manual `cargo build`, set the GCC include path (the install script does this automatically):

```bash
export C_INCLUDE_PATH="$(gcc -print-file-name=include)"
```

Validate the desktop entry (optional):

```bash
desktop-file-validate data/dev.alcedo.Mail.desktop
```

## Mock UI

Run the full 3-pane shell with fake mail and no network:

```bash
ALCEDO_MOCK=1 cargo run -p alcedo
```

## Dragging messages

Drag the list handle onto a folder to **move**. Hold **Ctrl** while dragging, or **right-click-drag** the handle, to **copy** (`UID COPY`).

Some IMAP servers treat `COPY` as a move (the message leaves the source folder). Gmail copies as expected. Alcedo does not fake a copy with `APPEND`.

## OAuth

Copy `oauth.toml.example` to `$XDG_CONFIG_HOME/alcedo/oauth.toml` and add your Google and/or Microsoft client IDs. Google Desktop OAuth clients also need `client_secret` (it is not confidential for installed apps):

```toml
[google]
client_id = "xxxxx.apps.googleusercontent.com"
client_secret = "GOCSPX-xxxxx"

[microsoft]
client_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
```

Yahoo and generic IMAP accounts use an app password stored in the system keyring via `oo7`.

## Logging

```bash
RUST_LOG=alcedo=debug cargo run -p alcedo
```

Use `RUST_LOG=alcedo=trace` for verbose sync and protocol detail.

## `mailto:` handler

When installed, Alcedo registers as the `mailto:` scheme handler. Launching
`alcedo mailto:user@example.com?subject=Hello` opens the composer with the
address and subject prefilled.

## Flatpak (development)

```bash
flatpak-builder --user --install flatpak_app build-aux/dev.alcedo.Mail.Devel.json
```

## License

Alcedo — Copyright (C) 2026 Alcedo contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version.

This program is distributed in the hope that it will be useful, but WITHOUT ANY
WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS FOR A
PARTICULAR PURPOSE. See the GNU General Public License for more details.

The full license text is in [LICENSE](LICENSE).

Additional permission under GNU GPL version 3 section 7:

If you modify this Program, or any covered work, by linking or combining it
with GTK, libadwaita, WebKitGTK, or other libraries licensed under LGPL, you
may convey the resulting combination under the terms of GPL-3.0-or-later.
