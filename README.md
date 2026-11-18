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

Release builds read the installed GSettings schema; install it with `./scripts/install-ubuntu-26.sh` or set `GSETTINGS_SCHEMA_DIR`.

Validate the desktop entry (optional):

```bash
desktop-file-validate data/dev.alcedo.Mail.desktop
```

## Debian package

Build the free app's `.deb` from a clean clone of this repository:

```bash
./scripts/build-deb.sh
```

The package and its `.sha256` file are written to `dist/`.

## OpenPGP library

By default Alcedo links its OpenPGP code (Sequoia with Nettle) into the binary.

A build made with `cargo build -p alcedo --no-default-features --features pgp-shared` instead loads `libalcedo_pgp_ffi.so` at run time. The library is searched for in this order:

1. The file named by the `ALCEDO_PGP_LIB` environment variable. If it is set, nothing else is tried.
2. `../lib/alcedo/libalcedo_pgp_ffi.so.1`, relative to the executable.
3. `libalcedo_pgp_ffi.so` next to the executable.
4. For `cargo test` binaries only, `libalcedo_pgp_ffi.so` one directory above the binary.

To rebuild the library from this source:

```bash
cargo build -p alcedo-pgp-ffi --release
```

To use the rebuilt library with an installed copy of Alcedo, set `ALCEDO_PGP_LIB=/path/to/target/release/libalcedo_pgp_ffi.so`, or replace the installed `libalcedo_pgp_ffi.so.1` with it.

To compile a modified Sequoia into it, add `--config 'patch.crates-io.sequoia-openpgp.path="/path/to/sequoia-openpgp"'`. Set `ALCEDO_PGP_BUILD_ID` while building to bake in an identifier that the client can report.

The interface between the client and the library is versioned. The library exports `alcedo_pgp_ffi_abi_version`, and the client refuses to use a library that reports a different version.

`scripts/pgp-relink-test.sh` rebuilds the library and runs the same, unmodified client test binary against it.

## Mock UI

Run the full 3-pane shell with fake mail and no network:

```bash
ALCEDO_MOCK=1 cargo run -p alcedo
```

## Dragging messages

Drag the list handle onto a folder to **move**. Hold **Ctrl** while dragging, or **right-click-drag** the handle, to **copy** (`UID COPY`).

Some IMAP servers treat `COPY` as a move (the message leaves the source folder). Gmail copies as expected. Alcedo does not fake a copy with `APPEND`.

## Gmail and other mail accounts

Gmail works with an app password and needs no configuration file:

1. Turn on 2-Step Verification for your Google account.
2. Create an app password under Google Account > Security > App passwords.
3. In Add account, enter your Gmail address, then set **Provider** to **Other email**. Alcedo fills in `imap.gmail.com` (port 993) and `smtp.gmail.com` (port 465, SSL/TLS). Enter the app password as the password.

Google Workspace admins can turn app passwords off. Yahoo and generic IMAP accounts also use app passwords, stored in the system keyring via `oo7`.

## Microsoft sign-in

Builds made with `ALCEDO_MICROSOFT_CLIENT_ID` set include that Microsoft public client, so Outlook.com and Microsoft 365 accounts sign in through the browser with no configuration file:

```bash
ALCEDO_MICROSOFT_CLIENT_ID="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx" cargo build --release -p alcedo
```

Register the client for personal and organizational accounts, as a public client with the redirect URI `http://127.0.0.1/`. Add that URI in the app manifest, because the Azure portal's redirect form rejects `http` loopback addresses. Builds without the variable need a `[microsoft]` entry in `oauth.toml`.

## OAuth (advanced, optional)

Google and Microsoft sign-in can use your own OAuth client IDs instead. For Google this needs a Google Cloud project with a desktop OAuth client. Copy `oauth.toml.example` to `$XDG_CONFIG_HOME/alcedo/oauth.toml` and add your client IDs. A `[microsoft]` entry replaces the built-in client, and accounts signed in with a different client must sign in again. Google Desktop OAuth clients also need `client_secret` (it is not confidential for installed apps):

```toml
[google]
client_id = "xxxxx.apps.googleusercontent.com"
client_secret = "GOCSPX-xxxxx"

[microsoft]
client_id = "xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
```


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
