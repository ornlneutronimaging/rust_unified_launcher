# Neutron Imaging Launcher (`rust_unified_launcher`)

One entry point for **every** imaging application: Rust GUIs, Jupyter portals,
marimo portals, and Python applications. Successor to
`portal_to_all_rust_applications`, generalized to all entry points and driven
by a config file instead of hardcoded entries.

## How it works

- `applications.toml` (in this repo) lists every application: name,
  description, category, argv command, optional preview screenshot, optional
  flags (`in_terminal`, `clear_fontconfig`, `requires_browser`, `check_path`,
  `workdir`, `tags`).
- The launcher renders them grouped by category with a search box, a preview
  panel, availability checks (missing targets are grayed out with the reason
  on hover), and a 5-second per-app launch cooldown.
- Commands are usually the existing `menu/start_*` or repo `launch_*.sh`
  scripts, so the launch logic stays in one place. Scripts are invoked through
  `/bin/bash` because some have no shebang line.
- An entry with `url = "https://..."` (instead of `command`) is a web link:
  it opens the page in the default browser (xdg-open / firefox, first found
  on PATH), is always shown as available, and its button reads **Open**.
- `in_terminal = true` wraps the command in gnome-terminal / konsole /
  xfce4-terminal / xterm (first found on PATH) so pixi/conda setup output
  stays visible; if none exists, the app is launched directly.
- `requires_browser = true` marks a tool that opens (or runs inside) a web
  browser: Jupyter / marimo portals, the web applications, dashboards. The
  Firefox profile lives on shared storage, so a browser running on any other
  analysis machine locks it and the launch fails with "Firefox is already
  running". For such tools the launcher reads the profile's `lock` symlink
  before launching and, when it names another machine, holds the launch back
  and pops a window with **🔧 Fix browser issue** (runs
  `list_and_fix_running_browser.sh kill` — kills your Firefox / Chrome /
  Jupyter on every analysis machine and resets the profile — then shows the
  report) and **Launch anyway**. The same button sits in the preview panel of
  every browser tool, and in the report window shown after a browser launch
  fails. When the flag is unset the launcher guesses from the command / tags;
  `requires_browser = false` turns the check off (used for the Fix tool
  itself).
- A `[[category]]` with `passwords = ["word1", "word2"]` shows as 🔒 in the
  sidebar and asks for one of those passwords (case-insensitive) before
  revealing its applications; until unlocked they are hidden from the All
  view, the search results and "Recently used". The unlock lasts for the
  session. Passwords live in plain text in the config, so this is a soft
  gate against casual browsing, not a security boundary.

- A top-level `usage_db = "/dir"` records every successful launch as one
  JSON line in `<usage_db>/records/<user>.jsonl` (epoch, local time, user,
  full name, application, category, host, command/url). Best effort on a background
  thread; the **Portal Usage Monitor** (`portal_usage_monitor`, restricted
  category) merges those files and shows the log and per-application bar
  chart. Remove the key to stop recording.

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
