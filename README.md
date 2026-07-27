# Neutron Imaging Launcher (`rust_unified_launcher`)

One entry point for **every** imaging application: Rust GUIs, Jupyter portals,
marimo portals, and Python applications. Successor to
`portal_to_all_rust_applications`, generalized to all entry points and driven
by a config file instead of hardcoded entries.

## How it works

- `applications.toml` (in this repo) lists every application: name,
  description, category, argv command, optional preview screenshot, optional
  flags (`in_terminal`, `clear_fontconfig`, `check_path`, `workdir`, `tags`).
- The launcher renders them grouped by category with a search box, a preview
  panel, availability checks (missing targets are grayed out with the reason
  on hover), and a 5-second per-app launch cooldown.
- Commands are usually the existing `menu/start_*` or repo `launch_*.sh`
  scripts, so the launch logic stays in one place. Scripts are invoked through
  `/bin/bash` because some have no shebang line.
- `in_terminal = true` wraps the command in gnome-terminal / konsole /
  xfce4-terminal / xterm (first found on PATH) so pixi/conda setup output
  stays visible; if none exists, the app is launched directly.

## Adding / editing an application

Edit `applications.toml`, then press **Reload config** in the running
launcher. No recompile needed. Drop a screenshot anywhere (e.g. `previews/`)
and point `preview` at it to populate the right-hand panel.

## Config resolution

1. First CLI argument
2. `$UNIFIED_LAUNCHER_CONFIG`
3. `/SNS/VENUS/shared/software/git/rust_unified_launcher/applications.toml`

## Build & deploy

```bash
cargo build --release
cp target/release/rust_unified_launcher /SNS/VENUS/shared/software/bin/
chmod 775 /SNS/VENUS/shared/software/bin/rust_unified_launcher
```

Or just use `./launch_unified_launcher.sh`, which rebuilds when sources
changed and then execs the binary (this is what the menu entry calls).

Menu entry: `/SNS/VENUS/shared/software/menu/start_application_launcher`.
