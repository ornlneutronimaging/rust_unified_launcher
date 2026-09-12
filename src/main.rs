//! Unified launcher for all neutron imaging entry points (Rust GUIs, Jupyter
//! portals, marimo portals, Python applications).
//!
//! Everything the launcher shows comes from `applications.toml` next to this
//! repository — adding, removing or editing an application never requires a
//! recompile. Each entry is a plain argv command (usually one of the existing
//! `menu/start_*` or repo `launch_*.sh` scripts), so the launch logic itself
//! stays where it always was.
//!
//! Keyboard driven: the search bar is focused at startup, ↑/↓ move the
//! selection through the filtered list, Enter launches it, Esc clears the
//! search (and closes the launcher when the search is already empty).

mod theme;
mod zoom;

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;

const DEFAULT_CONFIG: &str =
    "/SNS/VENUS/shared/software/git/rust_unified_launcher/applications.toml";
const LOGO_BYTES: &[u8] = include_bytes!("../logos/ImagingLogo.png");
const LOGO_MAX_HEIGHT: f32 = 56.0;
const PREVIEW_PANEL_WIDTH: f32 = 340.0;
const CATEGORY_PANEL_WIDTH: f32 = 210.0;
/// Seconds during which an app's Launch button stays disabled after a click.
const LAUNCH_COOLDOWN: f64 = 5.0;
/// Seconds after a launch during which a non-zero exit is reported as a
/// failure in the status bar (later exits are reaped silently).
const FAILURE_WINDOW: f64 = 15.0;
/// Script listing this user's Firefox/Chrome/Jupyter processes across the
/// shared analysis machines. The browser profile lives on shared NFS/GPFS
/// storage, so a browser running on ANY analysis node locks it and makes new
/// launches on this machine fail with "already running". Run in `list` mode
/// when a browser launch fails, to show WHERE the other browser is running.
/// It works over passwordless SSH, which the script sets up itself for users
/// who have no SSH key yet. (The "Fix browser issue" button no longer uses
/// the script's `kill` mode: it just resets the profile, see
/// `reset_firefox_profile`.)
const BROWSER_SCAN_SCRIPT: &str =
    "/SNS/VENUS/shared/software/bin/list_and_fix_running_browser.sh";
/// Number of `~/.mozilla.bak-<timestamp>` backups kept by "Fix browser issue".
const MOZILLA_BACKUPS_KEPT: usize = 3;
/// Seconds between checks of the config file's mtime (auto-reload).
const CONFIG_CHECK_PERIOD: f64 = 1.0;
/// Seconds between re-checks of every app's `check_path` availability.
const AVAILABILITY_PERIOD: f64 = 5.0;
/// Number of entries shown in the "Recently used" section.
const RECENT_SHOWN: usize = 5;

// ---------------------------------------------------------------------------
// Configuration (applications.toml)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Config {
    #[serde(default = "default_title")]
    title: String,
    #[serde(default = "default_subtitle")]
    subtitle: String,
    #[serde(default, rename = "category")]
    categories: Vec<Category>,
    #[serde(default, rename = "app")]
    apps: Vec<AppEntry>,
    /// Directory of the shared usage database. Every successful launch
    /// appends one JSON line to `<usage_db>/records/<user>.jsonl` (user, local
    /// date/time, application, category, host). Omit to disable recording.
    #[serde(default)]
    usage_db: Option<String>,
    /// Environments inspected by the "Show library versions" switch offered
    /// in an unlocked password-protected category (see `LibraryVersions`).
    #[serde(default)]
    library_versions: LibraryVersions,
}

fn default_title() -> String {
    "Neutron Imaging Launcher".to_owned()
}

fn default_subtitle() -> String {
    "Select the application you want to launch".to_owned()
}

#[derive(Deserialize)]
struct Category {
    id: String,
    name: String,
    /// Non-empty: the category is locked until one of these passwords is
    /// typed (case-insensitive). While locked its apps are hidden from the
    /// All view, the search results and "Recently used".
    #[serde(default)]
    passwords: Vec<String>,
}

/// `[library_versions]`: the admin "Show library versions" switch. Lists,
/// for each configured Python environment (pixi, conda, venv…), the version
/// of its installed packages — to check e.g. which NeuNorm the marimo
/// notebooks use versus the Jupyter ones.
#[derive(Deserialize, Clone, Default)]
struct LibraryVersions {
    /// Packages listed first in every environment, in this order, with
    /// "not installed" when missing (names compared case-insensitively,
    /// `-`, `_` and `.` treated alike). The other installed packages are
    /// behind the "All packages" section and the filter box.
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default, rename = "environment")]
    environments: Vec<EnvEntry>,
}

#[derive(Deserialize, Clone)]
struct EnvEntry {
    name: String,
    /// The environment's `python` executable.
    python: String,
}

#[derive(Deserialize)]
struct AppEntry {
    name: String,
    #[serde(default)]
    description: String,
    category: String,
    /// Optional sub-section shown as a smaller header inside the category.
    /// Apps sharing a section must be consecutive in the file — the header is
    /// emitted whenever the section of the listed apps changes.
    #[serde(default)]
    section: Option<String>,
    /// Argv of the process to spawn (first element is the executable).
    /// May be omitted when `url` is set.
    #[serde(default)]
    command: Vec<String>,
    /// Web page opened in the default browser instead of running `command`.
    #[serde(default)]
    url: Option<String>,
    /// Working directory for the spawned process; defaults to the directory
    /// of the checked path (see `check_path`).
    #[serde(default)]
    workdir: Option<String>,
    /// Absolute path of a screenshot shown in the preview panel.
    #[serde(default)]
    preview: Option<String>,
    /// Path whose existence decides availability. Defaults to the last
    /// element of `command` that starts with `/`.
    #[serde(default)]
    check_path: Option<String>,
    /// Run inside a terminal emulator so console output (pixi setup, etc.)
    /// stays visible.
    #[serde(default)]
    in_terminal: bool,
    /// With `in_terminal`: keep the window open after the command exits
    /// ("Press Enter to close"), for tools that print a report and quit.
    #[serde(default)]
    hold_terminal: bool,
    /// Remove `~/.cache/fontconfig` before launching (stale-cache workaround
    /// used by the egui portals).
    #[serde(default)]
    clear_fontconfig: bool,
    /// Extra keywords matched by the search box.
    #[serde(default)]
    tags: Vec<String>,
    /// The tool opens (or runs inside) a web browser. The Firefox profile
    /// lives on shared storage, so a browser running on another analysis
    /// machine makes such a launch fail: the profile lock is checked before
    /// launching and the preview panel offers "Fix browser issue". Unset:
    /// inferred from the command / tags (see `involves_browser`); `false`
    /// disables the check for tools that merely mention a browser.
    #[serde(default)]
    requires_browser: Option<bool>,
}

impl AppEntry {
    fn checked_path(&self) -> Option<PathBuf> {
        if let Some(p) = &self.check_path {
            return Some(PathBuf::from(p));
        }
        self.command
            .iter()
            .rev()
            .find(|a| a.starts_with('/'))
            .map(PathBuf::from)
    }

    fn available(&self) -> bool {
        if self.url.is_some() {
            return true;
        }
        !self.command.is_empty()
            && self.checked_path().map(|p| p.exists()).unwrap_or(true)
    }

    /// Spawn the app detached. Returns the child (watched for a few seconds
    /// to surface immediate failures) and the log file capturing its output.
    fn launch(&self) -> Result<(Child, Option<PathBuf>), String> {
        if self.command.is_empty() && self.url.is_none() {
            return Err(format!("{}: no command or url configured", self.name));
        }
        if self.clear_fontconfig {
            if let Some(home) = std::env::var_os("HOME") {
                let _ = std::fs::remove_dir_all(
                    Path::new(&home).join(".cache/fontconfig"),
                );
            }
        }

        let mut argv: Vec<String> = if let Some(url) = &self.url {
            let opener = find_url_opener().ok_or_else(|| {
                format!("{}: no browser opener (xdg-open/firefox) found", self.name)
            })?;
            vec![opener, url.clone()]
        } else {
            self.command.clone()
        };
        if self.in_terminal {
            if let Some((term, term_args)) = find_terminal() {
                let mut wrapped = vec![term];
                wrapped.extend(term_args);
                if self.hold_terminal {
                    let joined = argv
                        .iter()
                        .map(|a| shell_quote(a))
                        .collect::<Vec<_>>()
                        .join(" ");
                    wrapped.push("/bin/bash".to_owned());
                    wrapped.push("-c".to_owned());
                    wrapped.push(format!(
                        "{joined}; echo; read -r -p 'Press Enter to close...'"
                    ));
                } else {
                    wrapped.extend(argv);
                }
                argv = wrapped;
            } // no terminal emulator found: fall back to a plain spawn
        }

        let workdir = self
            .workdir
            .clone()
            .map(PathBuf::from)
            .or_else(|| {
                self.checked_path()
                    .and_then(|p| p.parent().map(PathBuf::from))
            })
            .unwrap_or_else(|| PathBuf::from("/"));

        // Capture the app's output in a per-app log so an immediate failure
        // has something to point at; fall back to /dev/null when the log
        // cannot be created (read-only home, etc.).
        let mut log_path: Option<PathBuf> = None;
        let mut stdio: Option<(Stdio, Stdio)> = None;
        if let Some(path) = launch_log_path(&self.name) {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(mut file) = std::fs::File::create(&path) {
                let _ = writeln!(file, "$ {}", argv.join(" "));
                if let Ok(clone) = file.try_clone() {
                    stdio = Some((Stdio::from(file), Stdio::from(clone)));
                    log_path = Some(path);
                }
            }
        }
        let (stdout, stderr) =
            stdio.unwrap_or_else(|| (Stdio::null(), Stdio::null()));

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(workdir)
            // Detach from the launcher's terminal so the app keeps running
            // (and stays quiet) after the portal exits.
            .stdin(Stdio::null())
            .stdout(stdout)
            .stderr(stderr);
        // Put the app in its own session so closing the portal (or its
        // terminal) never delivers SIGHUP/SIGINT/SIGTERM to launched apps.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn()
            .map(|child| (child, log_path))
            .map_err(|e| format!("Cannot launch {}: {e}", argv[0]))
    }

    /// Does launching this app open (or depend on) a web browser? Explicit
    /// `requires_browser` wins; otherwise a URL entry or a browser-ish word
    /// in the command / tags. Decides the pre-launch profile-lock check, the
    /// "Fix browser issue" button in the preview panel and whether a fast
    /// failure is worth a cross-machine browser scan (`BROWSER_SCAN_SCRIPT`).
    fn involves_browser(&self) -> bool {
        if let Some(explicit) = self.requires_browser {
            return explicit;
        }
        if self.url.is_some() {
            return true;
        }
        let needles = [
            "firefox", "chrome", "chromium", "browser", "jupyter", "marimo",
            "portal", "website", "web",
        ];
        self.command
            .iter()
            .chain(self.tags.iter())
            .any(|s| {
                let s = s.to_lowercase();
                needles.iter().any(|n| s.contains(n))
            })
    }

    /// Every whitespace-separated token of the needle must match the name,
    /// description or a tag — so "ct recon" finds "CT Reconstruction" no
    /// matter how the words are split across the fields.
    fn matches(&self, needle: &str) -> bool {
        needle.split_whitespace().all(|token| {
            let token = token.to_lowercase();
            self.name.to_lowercase().contains(&token)
                || self.description.to_lowercase().contains(&token)
                || self.tags.iter().any(|t| t.to_lowercase().contains(&token))
                || self
                    .url
                    .as_ref()
                    .map(|u| u.to_lowercase().contains(&token))
                    .unwrap_or(false)
        })
    }
}

/// Warnings for entries that reference a category id with no `[[category]]`
/// block — they would silently render under the raw id otherwise.
fn validate_config(cfg: &Config) -> Vec<String> {
    cfg.apps
        .iter()
        .filter(|a| !cfg.categories.iter().any(|c| c.id == a.category))
        .map(|a| {
            format!(
                "\"{}\" references unknown category \"{}\"",
                a.name, a.category
            )
        })
        .collect()
}

/// Does a captured launch log look like a "browser already running / profile
/// locked" failure? Catches apps not flagged by `involves_browser` whose
/// output still names the lock.
fn log_mentions_browser_lock(log: &Option<PathBuf>) -> bool {
    let Some(path) = log else { return false };
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let text = text.to_lowercase();
    ["already running", "is in use", "singleton", "parentlock", "profile directory"]
        .iter()
        .any(|n| text.contains(n))
}

/// Single-quote a string for safe interpolation into a `bash -c` command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Locate a program able to open a URL in the user's default browser.
fn find_url_opener() -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for name in ["xdg-open", "firefox", "google-chrome", "chromium-browser"] {
        for dir in std::env::split_paths(&path) {
            let full = dir.join(name);
            if full.is_file() {
                return Some(full.to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Locate a terminal emulator on PATH; returns (path, pre-command args).
fn find_terminal() -> Option<(String, Vec<String>)> {
    let candidates: [(&str, &[&str]); 4] = [
        ("gnome-terminal", &["--"]),
        ("konsole", &["-e"]),
        ("xfce4-terminal", &["-x"]),
        ("xterm", &["-e"]),
    ];
    let path = std::env::var_os("PATH")?;
    for (name, args) in candidates {
        for dir in std::env::split_paths(&path) {
            let full = dir.join(name);
            if full.is_file() {
                return Some((
                    full.to_string_lossy().into_owned(),
                    args.iter().map(|s| s.to_string()).collect(),
                ));
            }
        }
    }
    None
}

fn config_path() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    if let Ok(env) = std::env::var("UNIFIED_LAUNCHER_CONFIG") {
        return PathBuf::from(env);
    }
    PathBuf::from(DEFAULT_CONFIG)
}

fn load_config(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("Bad TOML in {}: {e}", path.display()))
}

fn config_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

// ---------------------------------------------------------------------------
// Per-user state (~/.cache/unified_launcher): launch history & logs
// ---------------------------------------------------------------------------

fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| Path::new(&home).join(".cache/unified_launcher"))
}

fn launch_log_path(app_name: &str) -> Option<PathBuf> {
    let safe: String = app_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    cache_dir().map(|d| d.join("logs").join(format!("{safe}.log")))
}

#[derive(Default, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default)]
    entries: HashMap<String, RecentEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct RecentEntry {
    count: u64,
    last_epoch: u64,
}

fn recent_path() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("recent.toml"))
}

fn load_recent() -> RecentFile {
    recent_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

/// Best effort: an unwritable home only costs the launch history.
fn save_recent(recent: &RecentFile) {
    let Some(path) = recent_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = toml::to_string(recent) {
        let _ = std::fs::write(path, text);
    }
}

// ---------------------------------------------------------------------------
// Shared usage database (`usage_db` in the config): one JSON line per launch
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct UsageRecord {
    /// Seconds since the Unix epoch (UTC).
    epoch: u64,
    /// Same instant as local ISO 8601 with UTC offset, e.g.
    /// `2026-09-09T13:42:07-04:00`.
    ts: String,
    user: String,
    /// Full name from the account database (GECOS field), empty if unknown.
    name: String,
    app: String,
    category: String,
    host: String,
    /// `command` or `url`.
    kind: &'static str,
}

fn current_user() -> String {
    ["USER", "LOGNAME"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| format!("uid{}", unsafe { libc::getuid() }))
}

/// Turn a GECOS field into a display name. Classic GECOS is
/// `Full Name,room,phone,...` (first part); this site stores `Last, First`
/// instead, which is recognised (one comma, no digits) and reordered to
/// `First Last`.
fn gecos_display_name(gecos: &str) -> String {
    let parts: Vec<&str> = gecos.split(',').map(str::trim).collect();
    match parts.as_slice() {
        [last, first] if !first.is_empty() && !gecos.chars().any(|c| c.is_ascii_digit()) => {
            format!("{first} {last}")
        }
        [first, ..] => (*first).to_owned(),
        [] => String::new(),
    }
}

/// Full name of the current user from the passwd database (GECOS field),
/// empty when unavailable.
fn current_full_name() -> String {
    let mut buf = vec![0u8; 16 * 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_gecos.is_null() {
        return String::new();
    }
    let gecos = unsafe { std::ffi::CStr::from_ptr(pwd.pw_gecos) };
    gecos_display_name(&gecos.to_string_lossy())
}

fn current_host() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Local ISO 8601 timestamp with UTC offset for the given epoch.
fn local_iso_timestamp(epoch: u64) -> String {
    let t = epoch as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    let sign = if tm.tm_gmtoff < 0 { '-' } else { '+' };
    let off = tm.tm_gmtoff.abs();
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec,
        sign,
        off / 3600,
        (off % 3600) / 60
    )
}

/// Append one launch record to `<usage_db>/records/<user>.jsonl`. One file
/// per user (in a sticky, world-writable directory) so concurrent launches
/// from different users and machines never write to the same file. Best
/// effort: any failure is silently ignored, a launch must never depend on
/// the shared filesystem being writable. Runs on its own thread so a slow
/// filesystem cannot stall the UI.
fn record_usage(
    usage_db: PathBuf,
    app_name: String,
    category: String,
    is_url: bool,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let epoch = epoch_now();
        let user = current_user();
        let record = UsageRecord {
            epoch,
            ts: local_iso_timestamp(epoch),
            user: user.clone(),
            name: current_full_name(),
            app: app_name,
            category,
            host: current_host(),
            kind: if is_url { "url" } else { "command" },
        };
        let Ok(line) = serde_json::to_string(&record) else { return };
        let dir = usage_db.join("records");
        let _ = std::fs::create_dir_all(&dir);
        let safe_user: String = user
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' })
            .collect();
        let path = dir.join(format!("{safe_user}.jsonl"));
        let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        else {
            return;
        };
        // The dashboard is built by another user: keep the file readable
        // whatever this user's umask is.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644));
        let _ = writeln!(file, "{line}");
    })
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn gecos_display_name_handles_site_and_classic_forms() {
        assert_eq!(gecos_display_name("Bilheux, Jean-Christophe"), "Jean-Christophe Bilheux");
        assert_eq!(gecos_display_name("Jane Doe,Room 12,555-1234,"), "Jane Doe");
        assert_eq!(gecos_display_name("Jane Doe"), "Jane Doe");
        assert_eq!(gecos_display_name(""), "");
        assert_eq!(gecos_display_name("Doe,"), "Doe");
    }

    #[test]
    fn record_usage_appends_one_json_line_per_launch() {
        let dir = std::env::temp_dir().join(format!(
            "unified_launcher_usage_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        record_usage(dir.clone(), "App One".into(), "rust".into(), false)
            .join()
            .unwrap();
        record_usage(dir.clone(), "Web \"Link\"".into(), "web".into(), true)
            .join()
            .unwrap();
        let path = dir.join("records").join(format!("{}.jsonl", current_user()));
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["app"], "App One");
        assert_eq!(first["category"], "rust");
        assert_eq!(first["kind"], "command");
        assert_eq!(first["user"], current_user());
        assert!(first["name"].is_string());
        println!("full name recorded as: {:?}", first["name"]);
        assert_eq!(first["host"], current_host());
        assert!(first["epoch"].as_u64().unwrap() > 1_700_000_000);
        let ts = first["ts"].as_str().unwrap();
        assert_eq!(ts.len(), 25, "{ts}");
        assert_eq!(&ts[10..11], "T");
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second["app"], "Web \"Link\"");
        assert_eq!(second["kind"], "url");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Textures
// ---------------------------------------------------------------------------

enum Preview {
    Missing,
    Loaded(egui::TextureHandle),
}

fn load_texture(ctx: &egui::Context, name: &str, bytes: &[u8]) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(bytes).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let pixels = rgba.into_raw();
    let color_image = egui::ColorImage::from_rgba_unmultiplied(size, &pixels);
    Some(ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR))
}

fn load_preview(ctx: &egui::Context, app: &AppEntry, idx: usize) -> Preview {
    let Some(path) = &app.preview else {
        return Preview::Missing;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return Preview::Missing;
    };
    match load_texture(ctx, &format!("preview_{idx}"), &bytes) {
        Some(tex) => Preview::Loaded(tex),
        None => Preview::Missing,
    }
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------
// Library versions (admin view)
// ---------------------------------------------------------------------------

/// Python snippet printing `{"python": "3.x.y", "packages": {name: version}}`
/// for every distribution visible to the interpreter it runs in.
const LIST_PACKAGES_PY: &str = r#"
import importlib.metadata as m, json, sys
pk = {}
for d in m.distributions():
    n = d.metadata['Name']
    if n:
        pk[n] = d.version
print(json.dumps({'python': sys.version.split()[0], 'packages': pk}))
"#;

/// Package-name normalisation (PEP 503): case-insensitive, `-`/`_`/`.` alike.
fn norm_pkg(name: &str) -> String {
    name.trim().to_lowercase().replace(['_', '.'], "-")
}

/// Python version + installed packages, sorted by name.
type PackageList = (String, Vec<(String, String)>);

fn list_env_packages(python: &str) -> Result<PackageList, String> {
    if !Path::new(python).is_file() {
        return Err("python executable not found".to_owned());
    }
    let out = Command::new(python)
        .args(["-c", LIST_PACKAGES_PY])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run python: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "python failed: {}",
            err.trim().lines().last().unwrap_or("(no output)")
        ));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("bad output: {e}"))?;
    let pyver = v["python"].as_str().unwrap_or("?").to_owned();
    let mut pk: Vec<(String, String)> = v["packages"]
        .as_object()
        .map(|o| {
            o.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("?").to_owned()))
                .collect()
        })
        .unwrap_or_default();
    pk.sort_by_key(|(n, _)| n.to_lowercase());
    Ok((pyver, pk))
}

/// Listing of one environment (`index` into `LibraryVersions::environments`).
struct EnvVersions {
    index: usize,
    result: Result<PackageList, String>,
}

/// Package listing of every configured environment, one thread each (a
/// python start-up on GPFS takes seconds; they run in parallel).
struct VersionsScan {
    rx: mpsc::Receiver<EnvVersions>,
    expected: usize,
    envs: Vec<EnvVersions>,
}

impl VersionsScan {
    fn start(lv: &LibraryVersions) -> Self {
        let (tx, rx) = mpsc::channel();
        for (index, env) in lv.environments.iter().enumerate() {
            let tx = tx.clone();
            let python = env.python.clone();
            std::thread::spawn(move || {
                let result = list_env_packages(&python);
                let _ = tx.send(EnvVersions { index, result });
            });
        }
        Self { rx, expected: lv.environments.len(), envs: Vec::new() }
    }

    fn poll(&mut self) {
        while let Ok(env) = self.rx.try_recv() {
            self.envs.push(env);
        }
    }

    fn running(&self) -> bool {
        self.envs.len() < self.expected
    }

    fn env(&self, index: usize) -> Option<&EnvVersions> {
        self.envs.iter().find(|e| e.index == index)
    }
}

/// Plain-text report (configured packages of every environment) for the
/// clipboard.
fn versions_report(lv: &LibraryVersions, scan: &VersionsScan) -> String {
    let mut out = String::new();
    for (i, env) in lv.environments.iter().enumerate() {
        out.push_str(&format!("{}\n  {}\n", env.name, env.python));
        match scan.env(i).map(|e| &e.result) {
            None => out.push_str("  (not read yet)\n"),
            Some(Err(e)) => out.push_str(&format!("  error: {e}\n")),
            Some(Ok((pyver, pk))) => {
                out.push_str(&format!("  python: {pyver}\n"));
                for want in &lv.packages {
                    let key = norm_pkg(want);
                    match pk.iter().find(|(n, _)| norm_pkg(n) == key) {
                        Some((n, v)) => out.push_str(&format!("  {n}: {v}\n")),
                        None => out.push_str(&format!("  {want}: not installed\n")),
                    }
                }
            }
        }
        out.push('\n');
    }
    out
}

/// iOS-style on/off switch.
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired = ui.spacing().interact_size.y * egui::vec2(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(desired, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        let fill = if *on { theme::PRIMARY } else { visuals.bg_fill };
        ui.painter()
            .rect(rect, radius, fill, visuals.bg_stroke, egui::StrokeKind::Inside);
        let cx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let center = egui::pos2(cx, rect.center().y);
        ui.painter()
            .circle(center, 0.75 * radius, visuals.fg_stroke.color, visuals.fg_stroke);
    }
    response
}

/// The library-versions panel (admin view). Returns whether a (re)scan was
/// requested.
fn versions_panel(
    ui: &mut egui::Ui,
    lv: &LibraryVersions,
    scan: Option<&VersionsScan>,
    filter: &mut String,
) -> bool {
    if lv.environments.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.label(
                egui::RichText::new(
                    "No environment configured: add a [library_versions] table with \
                     [[library_versions.environment]] entries to applications.toml",
                )
                .weak()
                .italics(),
            );
        });
        return false;
    }
    let mut refresh = false;
    let running = scan.map(|s| s.running()).unwrap_or(false);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(!running, egui::Button::new("⟳ Refresh"))
            .on_hover_text("Read the environments again")
            .clicked()
        {
            refresh = true;
        }
        if let Some(scan) = scan {
            if ui
                .add_enabled(!running, egui::Button::new("📋 Copy report"))
                .on_hover_text("Copy the versions of the listed packages to the clipboard")
                .clicked()
            {
                ui.ctx().copy_text(versions_report(lv, scan));
            }
        }
        if running {
            ui.spinner();
            let done = scan.map(|s| s.envs.len()).unwrap_or(0);
            ui.label(
                egui::RichText::new(format!(
                    "Reading environment {} of {}…",
                    done + 1,
                    lv.environments.len()
                ))
                .weak(),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if !filter.is_empty() && ui.small_button("✖").clicked() {
                filter.clear();
            }
            ui.add(
                egui::TextEdit::singleline(filter)
                    .hint_text("Filter packages (e.g. neunorm)")
                    .desired_width(220.0),
            );
        });
    });
    ui.add_space(6.0);

    let filt = norm_pkg(filter);
    egui::ScrollArea::vertical()
        .id_salt("versions_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (i, env) in lv.environments.iter().enumerate() {
                let result = scan.and_then(|s| s.env(i)).map(|e| &e.result);
                let title = match result {
                    Some(Ok((pyver, _))) => format!("{}  ·  Python {pyver}", env.name),
                    _ => env.name.clone(),
                };
                egui::CollapsingHeader::new(egui::RichText::new(title).strong())
                    .id_salt(("versions_env", i))
                    .default_open(true)
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(&env.python).weak().small());
                        ui.add_space(2.0);
                        match result {
                            None => {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.label(egui::RichText::new("Reading…").weak());
                                });
                            }
                            Some(Err(e)) => {
                                ui.colored_label(theme::DANGER, format!("✖ {e}"));
                            }
                            Some(Ok((_, pk))) => {
                                let mut shown: HashSet<String> = HashSet::new();
                                let mut rows = 0;
                                egui::Grid::new(("versions_grid", i))
                                    .num_columns(2)
                                    .spacing([28.0, 4.0])
                                    .striped(true)
                                    .show(ui, |ui| {
                                        for want in &lv.packages {
                                            let key = norm_pkg(want);
                                            if !filt.is_empty() && !key.contains(&filt) {
                                                continue;
                                            }
                                            shown.insert(key.clone());
                                            rows += 1;
                                            match pk.iter().find(|(n, _)| norm_pkg(n) == key) {
                                                Some((n, v)) => {
                                                    ui.label(egui::RichText::new(n).strong());
                                                    ui.label(
                                                        egui::RichText::new(v)
                                                            .color(theme::primary_text(ui.visuals()))
                                                            .strong(),
                                                    );
                                                }
                                                None => {
                                                    ui.label(egui::RichText::new(want).strong());
                                                    ui.label(
                                                        egui::RichText::new("not installed")
                                                            .weak()
                                                            .italics(),
                                                    );
                                                }
                                            }
                                            ui.end_row();
                                        }
                                        // The other packages only when filtering;
                                        // the full list is behind the section below.
                                        if !filt.is_empty() {
                                            for (n, v) in pk {
                                                let key = norm_pkg(n);
                                                if shown.contains(&key) || !key.contains(&filt) {
                                                    continue;
                                                }
                                                rows += 1;
                                                ui.label(n);
                                                ui.label(v);
                                                ui.end_row();
                                            }
                                        }
                                    });
                                if rows == 0 {
                                    ui.label(
                                        egui::RichText::new("No package matches the filter")
                                            .weak()
                                            .italics(),
                                    );
                                }
                                if filt.is_empty() {
                                    egui::CollapsingHeader::new(format!("All {} packages", pk.len()))
                                        .id_salt(("versions_all", i))
                                        .default_open(false)
                                        .show(ui, |ui| {
                                            egui::Grid::new(("versions_all_grid", i))
                                                .num_columns(2)
                                                .spacing([28.0, 2.0])
                                                .striped(true)
                                                .show(ui, |ui| {
                                                    for (n, v) in pk {
                                                        ui.label(n);
                                                        ui.label(v);
                                                        ui.end_row();
                                                    }
                                                });
                                        });
                                }
                            }
                        }
                    });
                ui.add_space(4.0);
            }
        });
    refresh
}

// ---------------------------------------------------------------------------

/// A launched child watched for a short while so an immediate crash shows up
/// in the status bar; kept afterwards only to reap it when it exits.
struct PendingLaunch {
    name: String,
    child: Child,
    started: f64,
    log: Option<PathBuf>,
    /// Launching this app opens a browser — a fast failure triggers the
    /// cross-machine "where is my browser already running?" scan.
    browserish: bool,
}

/// What the "browser" report window is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BrowserWindowKind {
    /// Checked before launching a browser tool: the Firefox profile lock
    /// names another analysis machine, so the launch was held back.
    LockedBeforeLaunch,
    /// A fast browser-related failure: where the browser is already running
    /// (profile lock symlink, or the cross-machine scan's report).
    Scan,
}

/// The pop-up window with a browser report and its action buttons.
struct BrowserWindow {
    kind: BrowserWindowKind,
    /// App whose launch triggered the window.
    app_name: String,
    report: String,
    /// `LockedBeforeLaunch`: index of the held-back app, for "Launch anyway".
    blocked: Option<usize>,
}

/// The scan report lists offending hosts as "[host] N process(es):" blocks.
fn scan_found_processes(report: &str) -> bool {
    report
        .lines()
        .any(|l| l.trim_start().starts_with('[') && l.contains("process(es):"))
}

/// Instant "where is my Firefox running" check: Firefox writes a `lock`
/// symlink inside each profile pointing to "ip:+pid" of the owning process,
/// so when the shared profile is locked by a session on ANOTHER machine the
/// symlink names that machine directly — no SSH scan needed. Returns a
/// report for the pop-up window, or None when no remote lock is held (a
/// lock held by this machine is harmless: firefox just opens a new tab).
fn firefox_remote_lock_report() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let profiles = Path::new(&home).join(".mozilla/firefox");
    let local_ips: Vec<String> = Command::new("hostname")
        .arg("-I")
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut blocks = Vec::new();
    for entry in std::fs::read_dir(&profiles).ok()?.flatten() {
        let Ok(target) = std::fs::read_link(entry.path().join("lock")) else {
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        let Some((ip, pid)) = target.split_once(":+") else {
            continue;
        };
        if ip.starts_with("127.") || local_ips.iter().any(|l| l == ip) {
            continue;
        }
        let host = Command::new("getent")
            .args(["hosts", ip])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .nth(1)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| ip.to_owned());
        blocks.push(format!(
            "[{host}] ({ip}) firefox PID {pid} holds the profile lock\n    \
             (profile {})\n    to close it by hand:  ssh {host}  then  kill {pid}",
            entry.file_name().to_string_lossy(),
        ));
    }
    if blocks.is_empty() {
        None
    } else {
        blocks.push(
            "If Firefox is NOT actually running there, the lock is stale:\n    \
             delete the 'lock' and '.parentlock' files in that profile folder\n    \
             under ~/.mozilla/firefox/ (or click Fix browser issue, which\n    \
             moves the whole ~/.mozilla aside)."
                .to_owned(),
        );
        Some(blocks.join("\n\n"))
    }
}

/// "Fix browser issue": move `~/.mozilla` aside so Firefox starts with a
/// fresh profile. The profile sits on the shared home directory, so a Firefox
/// left running on another analysis machine holds its lock and every launch
/// here fails with "Firefox is already running". Renaming the directory (to
/// `~/.mozilla.bak-<timestamp>`) unblocks this machine at once, without SSH
/// access to the other hosts and without killing anything there: the remote
/// Firefox keeps running on the renamed folder until it is closed. A rename
/// also works while files inside are held open (an `rm -rf` on NFS would fail
/// with "Device or resource busy"), and it keeps bookmarks and saved logins
/// recoverable. Older backups made by this function are pruned down to
/// `MOZILLA_BACKUPS_KEPT`. Returns the status-bar message.
fn reset_firefox_profile() -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_owned())?;
    let home = Path::new(&home);
    let profile = home.join(".mozilla");
    if std::fs::symlink_metadata(&profile).is_err() {
        return Ok("No ~/.mozilla found: the Firefox profile is already fresh — \
                   you can launch again"
            .to_owned());
    }
    let stamp = local_timestamp();
    let mut backup = home.join(format!(".mozilla.bak-{stamp}"));
    let mut n = 1;
    while std::fs::symlink_metadata(&backup).is_ok() {
        backup = home.join(format!(".mozilla.bak-{stamp}-{n}"));
        n += 1;
    }
    std::fs::rename(&profile, &backup)
        .map_err(|e| format!("Could not move {} aside: {e}", profile.display()))?;
    prune_mozilla_backups(home);
    Ok(format!(
        "Firefox profile reset (moved to {}) — you can launch again",
        backup.display()
    ))
}

/// Keep only the newest `MOZILLA_BACKUPS_KEPT` of the `~/.mozilla.bak-*`
/// folders created by `reset_firefox_profile` (the names sort by time).
/// A plain `~/.mozilla.bak` made by hand is never touched.
fn prune_mozilla_backups(home: &Path) {
    let Ok(entries) = std::fs::read_dir(home) else { return };
    let mut backups: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".mozilla.bak-"))
        })
        .collect();
    backups.sort();
    for old in backups.iter().rev().skip(MOZILLA_BACKUPS_KEPT) {
        let _ = std::fs::remove_dir_all(old);
    }
}

/// Local time as `YYYYmmdd-HHMMSS` (via `date`, so the system time zone is
/// honoured without a chrono dependency); UTC epoch seconds as a fallback.
fn local_timestamp() -> String {
    Command::new("date")
        .arg("+%Y%m%d-%H%M%S")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|t| t.len() == 15)
        .unwrap_or_else(|| epoch_now().to_string())
}

#[cfg(test)]
mod profile_reset_tests {
    use super::*;

    #[test]
    fn prune_keeps_newest_backups_and_hand_made_one() {
        let home = std::env::temp_dir().join(format!("launcher_prune_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        for name in [
            ".mozilla.bak",
            ".mozilla.bak-20260901-120000",
            ".mozilla.bak-20260902-120000",
            ".mozilla.bak-20260903-120000",
            ".mozilla.bak-20260904-120000",
            ".mozilla.bak-20260905-120000",
        ] {
            std::fs::create_dir_all(home.join(name).join("firefox")).unwrap();
        }
        prune_mozilla_backups(&home);
        let mut left: Vec<String> = std::fs::read_dir(&home)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                ".mozilla.bak",
                ".mozilla.bak-20260903-120000",
                ".mozilla.bak-20260904-120000",
                ".mozilla.bak-20260905-120000",
            ]
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn timestamp_is_sortable() {
        let t = local_timestamp();
        assert_eq!(t.len(), 15, "{t}");
        assert_eq!(&t[8..9], "-");
        assert!(t.chars().filter(|c| c.is_ascii_digit()).count() == 14);
    }
}

/// The purple "🔧 Fix browser issue" button shared by the preview panel and
/// the report windows (same look as in the Jupyter / marimo portals).
/// Returns whether it was clicked.
fn fix_browser_button(ui: &mut egui::Ui) -> bool {
    ui.add(
        egui::Button::new(
            egui::RichText::new("\u{1F527} Fix browser issue").color(theme::TEXT_WHITE),
        )
        .fill(egui::Color32::from_rgb(138, 43, 226))
        .corner_radius(6.0)
        .min_size(egui::vec2(150.0, 28.0)),
    )
    .on_hover_text(
        "Move your Firefox profile (~/.mozilla) aside so Firefox starts fresh \
         here; the old profile is kept as ~/.mozilla.bak-<date>. Nothing is \
         killed on the other machines.",
    )
    .clicked()
}

struct App {
    config_path: PathBuf,
    config: Result<Config, String>,
    /// Mtime of the config when it was last (re)loaded, for auto-reload.
    config_mtime: Option<std::time::SystemTime>,
    config_warnings: Vec<String>,
    logo: Option<egui::TextureHandle>,
    available: Vec<bool>,
    previews: HashMap<usize, Preview>,
    /// Index into apps of the entry shown in the preview panel (last hovered
    /// or keyboard-selected).
    selected: Option<usize>,
    /// Index into apps of the keyboard selection (↑/↓, launched by Enter).
    highlighted: Option<usize>,
    /// Scroll the highlighted row into view this frame (set on ↑/↓).
    scroll_to_highlight: bool,
    /// Category id filter; `None` shows every category.
    active_category: Option<String>,
    search: String,
    /// Give the search bar keyboard focus on the next frame (set at startup).
    focus_search: bool,
    status: Option<Result<String, String>>,
    /// Per-app time (egui clock) of the last launch, for the cooldown.
    last_launch: HashMap<usize, f64>,
    /// Launch history (per app name), shown as "Recently used".
    recent: RecentFile,
    /// Ids of password-protected categories unlocked this session.
    unlocked: HashSet<String>,
    /// Contents of the password prompt shown for a locked category.
    password_input: String,
    /// A wrong password was just entered (shows the error line).
    password_wrong: bool,
    /// Give the password field keyboard focus on the next frame.
    focus_password: bool,
    /// Children being watched / reaped.
    pending: Vec<PendingLaunch>,
    /// Running cross-machine scan (app whose failure started it, report
    /// channel); see `BROWSER_SCAN_SCRIPT`.
    browser_scan: Option<(String, mpsc::Receiver<String>)>,
    /// The browser report window, when shown.
    browser_window: Option<BrowserWindow>,
    /// Admin switch: show the library versions instead of the app list.
    show_versions: bool,
    /// Package filter box of the library-versions panel.
    versions_filter: String,
    /// Package listing of every configured environment (running or done).
    versions: Option<VersionsScan>,
    /// "Launch anyway" was clicked for this app: skip the profile-lock check
    /// on the next launch.
    launch_anyway: Option<usize>,
    /// Egui times of the last config-mtime / availability checks.
    last_config_check: f64,
    last_availability_check: f64,
}

impl App {
    fn new(config_path: PathBuf) -> Self {
        let mut app = Self {
            config_path,
            config: Err(String::new()),
            config_mtime: None,
            config_warnings: Vec::new(),
            logo: None,
            available: Vec::new(),
            previews: HashMap::new(),
            selected: None,
            highlighted: None,
            scroll_to_highlight: false,
            active_category: None,
            search: String::new(),
            focus_search: true,
            status: None,
            last_launch: HashMap::new(),
            recent: load_recent(),
            unlocked: HashSet::new(),
            password_input: String::new(),
            password_wrong: false,
            focus_password: false,
            pending: Vec::new(),
            browser_scan: None,
            browser_window: None,
            show_versions: false,
            versions_filter: String::new(),
            versions: None,
            launch_anyway: None,
            last_config_check: 0.0,
            last_availability_check: 0.0,
        };
        app.reload(false);
        app
    }

    /// (Re)load the config, keeping the search, category filter and — when
    /// the app still exists — the preview/keyboard selection.
    fn reload(&mut self, announce: bool) {
        let selected_name = self.app_name(self.selected);
        let highlighted_name = self.app_name(self.highlighted);
        self.config = load_config(&self.config_path);
        self.config_mtime = config_mtime(&self.config_path);
        self.previews.clear();
        match &self.config {
            Ok(cfg) => {
                self.available = cfg.apps.iter().map(|a| a.available()).collect();
                self.config_warnings = validate_config(cfg);
                let position = |name: Option<String>| {
                    name.and_then(|n| cfg.apps.iter().position(|a| a.name == n))
                };
                self.selected = position(selected_name);
                self.highlighted = position(highlighted_name);
                if let Some(cat) = &self.active_category {
                    if !cfg.categories.iter().any(|c| &c.id == cat) {
                        self.active_category = None;
                    }
                }
            }
            Err(_) => {
                self.available.clear();
                self.config_warnings.clear();
                self.selected = None;
                self.highlighted = None;
            }
        }
        if announce {
            self.status = Some(Ok("Configuration reloaded".to_owned()));
        }
    }

    fn app_name(&self, idx: Option<usize>) -> Option<String> {
        match (&self.config, idx) {
            (Ok(cfg), Some(idx)) => cfg.apps.get(idx).map(|a| a.name.clone()),
            _ => None,
        }
    }

    /// Watch children: report a fast non-zero exit, silently reap the rest.
    /// A fast browser-related failure additionally starts the cross-machine
    /// scan showing where the already-running browser lives.
    fn poll_pending(&mut self, now: f64) {
        let mut failure: Option<(String, String, Option<PathBuf>, bool)> = None;
        self.pending.retain_mut(|p| match p.child.try_wait() {
            Ok(Some(exit)) => {
                if !exit.success() && now - p.started < FAILURE_WINDOW {
                    let code = exit
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "killed by signal".to_owned());
                    failure =
                        Some((p.name.clone(), code, p.log.clone(), p.browserish));
                }
                false
            }
            Ok(None) => true,
            Err(_) => false,
        });
        let Some((name, code, log, browserish)) = failure else { return };
        let mut msg = match &log {
            Some(log) => {
                format!("{name} exited (code {code}) — see {}", log.display())
            }
            None => format!("{name} exited (code {code})"),
        };
        if browserish || log_mentions_browser_lock(&log) {
            // The profile lock symlink answers instantly when present; only
            // fall back to the (slow, SSH-based) scan when it says nothing.
            if let Some(report) = firefox_remote_lock_report() {
                self.browser_window = Some(BrowserWindow {
                    kind: BrowserWindowKind::Scan,
                    app_name: name,
                    report,
                    blocked: None,
                });
                msg.push_str(
                    " — your browser is running on another machine (see the report window)",
                );
            } else if self.start_browser_scan(&name) {
                msg.push_str(
                    " — scanning the analysis machines for an already-running browser…",
                );
            }
        }
        self.status = Some(Err(msg));
    }

    /// Start `BROWSER_SCAN_SCRIPT` (list mode) in a background thread.
    /// Returns whether a scan was actually started (the script may be missing,
    /// or a scan may already be running).
    fn start_browser_scan(&mut self, app_name: &str) -> bool {
        if self.browser_scan.is_some() {
            return false; // one scan at a time
        }
        if !Path::new(BROWSER_SCAN_SCRIPT).is_file() {
            return false;
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let report = match Command::new("/bin/bash")
                .arg(BROWSER_SCAN_SCRIPT)
                .arg("list")
                .output()
            {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout);
                    if stdout.trim().is_empty() {
                        String::from_utf8_lossy(&out.stderr).into_owned()
                    } else {
                        // Drop the script's closing "Re-run with 'kill'" hint:
                        // the window advises closing the browser on the listed
                        // machine instead (remote kills are unreliable).
                        match stdout.find("Re-run with 'kill'") {
                            Some(pos) => stdout[..pos].trim_end().to_owned(),
                            None => stdout.into_owned(),
                        }
                    }
                }
                Err(e) => format!("Could not run the scan script: {e}"),
            };
            let _ = tx.send(report);
        });
        self.browser_scan = Some((app_name.to_owned(), rx));
        true
    }

    /// Collect a finished scan's report; pop the window when it found the
    /// user's browser running somewhere.
    fn poll_browser_scan(&mut self) {
        let Some((app_name, rx)) = &self.browser_scan else { return };
        let report = match rx.try_recv() {
            Ok(report) => report,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.browser_scan = None;
                return;
            }
        };
        let app_name = app_name.clone();
        self.browser_scan = None;
        if scan_found_processes(&report) {
            self.status = Some(Err(format!(
                "{app_name}: your browser is already running on another machine \
                 (see the report window)"
            )));
            self.browser_window = Some(BrowserWindow {
                kind: BrowserWindowKind::Scan,
                app_name,
                report,
                blocked: None,
            });
        } else {
            self.status = Some(Ok(
                "No already-running Firefox/Chrome/Jupyter found on the analysis machines"
                    .to_owned(),
            ));
        }
    }

    /// "Fix browser issue": reset the Firefox profile (see
    /// `reset_firefox_profile`), report in the status bar and close the
    /// browser report window — its lock information is obsolete once the
    /// profile has been moved aside, and the user just launches again.
    fn fix_browser(&mut self) {
        self.status = Some(reset_firefox_profile());
        if self.status.as_ref().is_some_and(Result::is_ok) {
            self.browser_window = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Application row (shared by the category list and "Recently used")
// ---------------------------------------------------------------------------

/// Returns the row's frame response and whether Launch was clicked.
fn app_row(
    ui: &mut egui::Ui,
    app: &AppEntry,
    available: bool,
    cooling: bool,
    highlighted: bool,
) -> (egui::Response, bool) {
    let mut clicked = false;
    let mut frame = egui::Frame::group(ui.style())
        .corner_radius(6.0)
        .inner_margin(10.0)
        .fill(theme::surface_weak(ui.visuals()));
    if highlighted {
        frame = frame.stroke(egui::Stroke::new(2.0, theme::PRIMARY));
    }
    let group = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(ui.available_width() - 120.0);
                ui.label(egui::RichText::new(&app.name).strong().size(16.0));
                ui.label(
                    egui::RichText::new(&app.description)
                        .color(theme::text_emphasis(ui.visuals())),
                );
            });
            ui.with_layout(
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.add_enabled_ui(available && !cooling, |ui| {
                        let label = if cooling {
                            "Launching..."
                        } else if app.url.is_some() {
                            "Open"
                        } else {
                            "Launch"
                        };
                        let mut button = egui::Button::new(
                            egui::RichText::new(label)
                                .color(theme::TEXT_WHITE)
                                .strong(),
                        )
                        .corner_radius(6.0)
                        .min_size(egui::vec2(100.0, 30.0));
                        if available && !cooling {
                            button = button.fill(theme::PRIMARY_RICH);
                        }
                        let hover = app.url.clone().unwrap_or_else(|| {
                            app.checked_path()
                                .map(|p| p.display().to_string())
                                .unwrap_or_else(|| app.command.join(" "))
                        });
                        let resp = ui
                            .add(button)
                            .on_hover_text(&hover)
                            .on_disabled_hover_text(if cooling {
                                "Starting, please wait...".to_owned()
                            } else {
                                format!("Not found: {hover}")
                            });
                        if resp.clicked() {
                            clicked = true;
                        }
                    });
                },
            );
        });
    });
    (group.response, clicked)
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.logo.is_none() {
            self.logo = load_texture(ctx, "imaging_logo", LOGO_BYTES);
        }
        let now = ctx.input(|i| i.time);

        // ------------------------------------------------ global keyboard --
        // Consumed before any widget sees them, so the focused search bar
        // never swallows the list navigation keys.
        let (move_up, move_down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });
        if escape {
            if self.search.is_empty() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                self.search.clear();
                self.focus_search = true;
            }
        }

        // ------------------------------------------- background housekeeping
        self.poll_pending(now);
        self.poll_browser_scan();
        if let Some(scan) = &mut self.versions {
            scan.poll();
            if scan.running() {
                ctx.request_repaint_after(std::time::Duration::from_millis(200));
            }
        }
        if now - self.last_config_check >= CONFIG_CHECK_PERIOD {
            self.last_config_check = now;
            if config_mtime(&self.config_path) != self.config_mtime {
                self.reload(false);
                self.status =
                    Some(Ok("Configuration reloaded (file changed)".to_owned()));
            } else if now - self.last_availability_check >= AVAILABILITY_PERIOD {
                // Tools appear/disappear (installs, NFS mounts) without the
                // config changing — refresh the greyed-out state too.
                self.last_availability_check = now;
                if let Ok(cfg) = &self.config {
                    self.available =
                        cfg.apps.iter().map(|a| a.available()).collect();
                }
            }
        }

        // ------------------------------------------------ branded header ---
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::new()
                    .fill(theme::PRIMARY_RICH)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let title = match &self.config {
                            Ok(cfg) => cfg.title.clone(),
                            Err(_) => default_title(),
                        };
                        let subtitle = match &self.config {
                            Ok(cfg) => cfg.subtitle.clone(),
                            Err(_) => default_subtitle(),
                        };
                        ui.label(
                            egui::RichText::new(title)
                                .strong()
                                .size(22.0)
                                .color(theme::TEXT_WHITE),
                        );
                        ui.label(
                            egui::RichText::new(subtitle).color(theme::TEXT_WHITE),
                        );
                    });
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            if let Some(tex) = &self.logo {
                                ui.add(
                                    egui::Image::from_texture(tex)
                                        .max_height(LOGO_MAX_HEIGHT),
                                );
                            }
                        },
                    );
                });
            });

        // --------------------------------------------------- status bar ---
        egui::TopBottomPanel::bottom("bottom").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .small_button("Reload config")
                    .on_hover_text(self.config_path.display().to_string())
                    .clicked()
                {
                    self.reload(true);
                }
                ui.separator();
                match &self.status {
                    Some(Ok(msg)) => {
                        ui.colored_label(theme::SUCCESS, msg);
                    }
                    Some(Err(msg)) => {
                        ui.colored_label(theme::DANGER, msg);
                    }
                    None => {
                        ui.colored_label(theme::text_emphasis(ui.visuals()), "Ready");
                    }
                }
                if !self.config_warnings.is_empty() {
                    ui.separator();
                    ui.colored_label(
                        theme::WARNING,
                        format!("⚠ {}", self.config_warnings.join("; ")),
                    )
                    .on_hover_text(
                        "Fix the category ids in applications.toml",
                    );
                }
            });
            ui.add_space(4.0);
        });

        // A config error replaces the whole body.
        let cfg = match &self.config {
            Ok(cfg) => cfg,
            Err(msg) => {
                let msg = msg.clone();
                egui::CentralPanel::default().show(ctx, |ui| {
                    // A long TOML error can outgrow a short window; scroll
                    // instead of clipping the advice at the bottom.
                    egui::ScrollArea::vertical()
                        .id_salt("config_error_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.add_space(24.0);
                            ui.vertical_centered(|ui| {
                                ui.colored_label(
                                    theme::DANGER,
                                    "Cannot load the application list",
                                );
                                ui.add_space(8.0);
                                ui.label(msg);
                                ui.add_space(8.0);
                                ui.label("Fix the file, then press \"Reload config\" below.");
                            });
                        });
                });
                // Keep watching the file so the fix is picked up on its own.
                ctx.request_repaint_after(std::time::Duration::from_secs(1));
                return;
            }
        };

        // Categories whose apps stay hidden until their password is typed.
        let locked: HashSet<&str> = cfg
            .categories
            .iter()
            .filter(|c| !c.passwords.is_empty() && !self.unlocked.contains(&c.id))
            .map(|c| c.id.as_str())
            .collect();

        // ------------------------------------------------- search bar ------
        // Full-width bar under the header; the list below narrows live as the
        // user types. Focused at startup so typing filters right away.
        egui::TopBottomPanel::top("search_bar")
            .frame(
                egui::Frame::new()
                    .fill(ctx.style().visuals.panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🔍").size(16.0));
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            theme::toggle_button(ui);
                            zoom::toggle_button(ui);
                            if !self.search.is_empty()
                                && ui
                                    .small_button("✖")
                                    .on_hover_text("Clear search (Esc)")
                                    .clicked()
                            {
                                self.search.clear();
                            }
                            let response = ui.add(
                                egui::TextEdit::singleline(&mut self.search)
                                    // Stable id: the ✖ button appearing/disappearing
                                    // before this widget must not change its identity,
                                    // or focus is lost after the first character.
                                    .id(egui::Id::new("search_field"))
                                    .hint_text(
                                        "Type to filter — arrow keys select, Enter launches, Esc clears",
                                    )
                                    .desired_width(ui.available_width()),
                            );
                            if self.focus_search {
                                response.request_focus();
                                self.focus_search = false;
                            }
                        },
                    );
                });
            });

        // -------------------------------------------- keyboard navigation --
        // The filtered list (indices into cfg.apps), in display order.
        let visible: Vec<usize> = cfg
            .apps
            .iter()
            .enumerate()
            .filter(|(_, a)| {
                !locked.contains(a.category.as_str())
                    && self
                        .active_category
                        .as_deref()
                        .map(|c| a.category == c)
                        .unwrap_or(true)
                    && a.matches(&self.search)
            })
            .map(|(i, _)| i)
            .collect();

        // The "Recently used" rows shown at the top of the All view. Apps
        // listed there are dropped from their category group below so every
        // app appears exactly once.
        let mut recent_rows: Vec<usize> = Vec::new();
        if self.active_category.is_none() && self.search.is_empty() {
            // An app can be listed in several categories under the same
            // name; keep only its first config entry so the recent section
            // never shows duplicates.
            let mut seen: Vec<&str> = Vec::new();
            let mut by_time: Vec<(usize, u64)> = cfg
                .apps
                .iter()
                .enumerate()
                .filter_map(|(i, a)| {
                    if locked.contains(a.category.as_str())
                        || seen.contains(&a.name.as_str())
                    {
                        return None;
                    }
                    seen.push(a.name.as_str());
                    self.recent.entries.get(&a.name).map(|e| (i, e.last_epoch))
                })
                .collect();
            by_time.sort_by(|a, b| b.1.cmp(&a.1));
            by_time.truncate(RECENT_SHOWN);
            recent_rows = by_time.into_iter().map(|(i, _)| i).collect();
        }
        let recent_names: Vec<&str> = recent_rows
            .iter()
            .map(|&i| cfg.apps[i].name.as_str())
            .collect();

        // Keyboard order follows the display: recent rows first, then the
        // category groups (minus the apps already shown as recent).
        let display_order: Vec<usize> = recent_rows
            .iter()
            .copied()
            .chain(
                visible
                    .iter()
                    .copied()
                    .filter(|&i| !recent_names.contains(&cfg.apps[i].name.as_str())),
            )
            .collect();

        let mut launch_request: Option<usize> = None;
        if (move_up || move_down) && !display_order.is_empty() {
            let pos = self
                .highlighted
                .and_then(|h| display_order.iter().position(|&i| i == h));
            let new_pos = match pos {
                Some(p) if move_down => (p + 1).min(display_order.len() - 1),
                Some(p) => p.saturating_sub(1),
                None => 0,
            };
            self.highlighted = Some(display_order[new_pos]);
            self.selected = self.highlighted;
            self.scroll_to_highlight = true;
        }
        if enter {
            // Enter launches the keyboard selection; with none, the first
            // match — but only while searching, so a stray Enter on the
            // full list never launches something by accident.
            launch_request = self
                .highlighted
                .filter(|h| visible.contains(h))
                .or_else(|| {
                    if self.search.is_empty() {
                        None
                    } else {
                        visible.first().copied()
                    }
                });
        }

        // ---------------------------------------------- category sidebar ---
        let mut clicked_category: Option<Option<String>> = None;
        egui::SidePanel::left("categories")
            .exact_width(CATEGORY_PANEL_WIDTH)
            .resizable(false)
            .show(ctx, |ui| {
                // Many categories (or the large-text mode) can outgrow a
                // short window; scroll instead of clipping the last ones.
                egui::ScrollArea::vertical()
                    .id_salt("categories_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.add_space(12.0);
                        ui.label(theme::section_heading("Categories"));
                        ui.add_space(6.0);
                        let total = cfg
                            .apps
                            .iter()
                            .filter(|a| !locked.contains(a.category.as_str()))
                            .count();
                        if ui
                            .selectable_label(
                                self.active_category.is_none(),
                                format!("All applications  ({total})"),
                            )
                            .clicked()
                        {
                            clicked_category = Some(None);
                        }
                        for cat in &cfg.categories {
                            let count =
                                cfg.apps.iter().filter(|a| a.category == cat.id).count();
                            let is_active =
                                self.active_category.as_deref() == Some(cat.id.as_str());
                            let label = if locked.contains(cat.id.as_str()) {
                                format!("🔒 {}", cat.name)
                            } else {
                                format!("{}  ({count})", cat.name)
                            };
                            if ui.selectable_label(is_active, label).clicked() {
                                clicked_category = Some(Some(cat.id.clone()));
                            }
                        }
                    });
            });

        // "Fix browser issue" clicks (preview panel, report window) are
        // collected here and started after the config borrow ends.
        let mut fix_clicked = false;

        // ------------------------------------------------- preview panel ---
        egui::SidePanel::right("preview_panel")
            .exact_width(PREVIEW_PANEL_WIDTH)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(12.0);
                let Some(idx) = self.selected else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label(
                            egui::RichText::new("Hover an application to preview it")
                                .weak()
                                .italics(),
                        );
                    });
                    return;
                };
                let Some(app) = cfg.apps.get(idx) else {
                    return;
                };
                // The screenshot adapts to the remaining height, but a long
                // description can outgrow a short window. The panel height
                // is measured before entering the scroll area — inside it
                // the available height is unbounded — and the minimum image
                // height below is what makes the scroll bar appear.
                let panel_h = ui.available_height();
                egui::ScrollArea::vertical()
                    .id_salt("preview_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new(&app.name).strong().size(16.0));
                        });
                        ui.add_space(6.0);
                        if !app.description.is_empty() {
                            ui.label(
                                egui::RichText::new(&app.description)
                                    .color(theme::text_emphasis(ui.visuals())),
                            );
                            ui.add_space(4.0);
                        }
                        if !app.tags.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("Tags: {}", app.tags.join(", ")))
                                    .weak()
                                    .small(),
                            );
                            ui.add_space(4.0);
                        }
                        ui.label(
                            egui::RichText::new(
                                app.url.clone().unwrap_or_else(|| app.command.join(" ")),
                            )
                            .weak()
                            .small()
                            .monospace(),
                        );
                        if app.involves_browser() {
                            // Browser tools fail when the shared Firefox
                            // profile is locked by a session on another
                            // machine: offer the fix right where the tool
                            // is launched from.
                            ui.add_space(8.0);
                            ui.label(
                                egui::RichText::new(
                                    "\u{1F310} Opens in your web browser. If the \
                                     browser refuses to open (\"Firefox is \
                                     already running\"), fix it here first:",
                                )
                                .small()
                                .color(theme::text_emphasis(ui.visuals())),
                            );
                            ui.add_space(4.0);
                            if fix_browser_button(ui) {
                                fix_clicked = true;
                            }
                        }
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(8.0);
                        if !self.previews.contains_key(&idx) {
                            let preview = load_preview(ctx, app, idx);
                            self.previews.insert(idx, preview);
                        }
                        let image_h = (panel_h
                            - ui.min_rect().height()
                            - ui.spacing().item_spacing.y
                            - 12.0)
                            .max(120.0);
                        match self.previews.get(&idx) {
                            Some(Preview::Loaded(tex)) => {
                                ui.vertical_centered(|ui| {
                                    ui.add(
                                        egui::Image::from_texture(tex)
                                            .max_width(ui.available_width())
                                            .max_height(image_h),
                                    );
                                });
                            }
                            _ => {
                                ui.vertical_centered(|ui| {
                                    ui.add_space(24.0);
                                    ui.label(
                                        egui::RichText::new("No preview available")
                                            .weak()
                                            .italics(),
                                    );
                                });
                            }
                        }
                    });
            });

        // ------------------------------------------------- application list
        let mut versions_request = false;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);

            // A locked category shows a password prompt instead of its apps.
            let locked_cat = self
                .active_category
                .as_deref()
                .filter(|c| locked.contains(*c))
                .and_then(|c| cfg.categories.iter().find(|cat| cat.id == c));
            if let Some(cat) = locked_cat {
                // The prompt is taller than a very short window (especially
                // in the large-text mode); scroll instead of clipping the
                // Unlock button off the bottom.
                egui::ScrollArea::vertical()
                    .id_salt("unlock_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| ui.vertical_centered(|ui| {
                    ui.add_space(48.0);
                    ui.label(egui::RichText::new("🔒").size(40.0));
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!("{} is protected", cat.name))
                            .strong()
                            .size(18.0),
                    );
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new("Enter the password to show its applications")
                            .color(theme::text_emphasis(ui.visuals())),
                    );
                    ui.add_space(12.0);
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut self.password_input)
                            .password(true)
                            .hint_text("Password")
                            .desired_width(220.0),
                    );
                    if self.focus_password {
                        response.request_focus();
                        self.focus_password = false;
                    }
                    ui.add_space(8.0);
                    // `enter` is consumed globally before any widget sees it;
                    // while this prompt is shown the app list is empty, so it
                    // can only mean "submit the password".
                    let submitted = ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("Unlock")
                                    .color(theme::TEXT_WHITE)
                                    .strong(),
                            )
                            .fill(theme::PRIMARY_RICH)
                            .corner_radius(6.0)
                            .min_size(egui::vec2(100.0, 30.0)),
                        )
                        .clicked()
                        || enter;
                    if submitted {
                        let typed = self.password_input.trim().to_lowercase();
                        if !typed.is_empty()
                            && cat
                                .passwords
                                .iter()
                                .any(|p| p.to_lowercase() == typed)
                        {
                            self.unlocked.insert(cat.id.clone());
                            self.password_wrong = false;
                            self.status =
                                Some(Ok(format!("{} unlocked", cat.name)));
                        } else {
                            self.password_wrong = true;
                            self.focus_password = true;
                        }
                        self.password_input.clear();
                    }
                    if self.password_wrong {
                        ui.add_space(8.0);
                        ui.colored_label(theme::DANGER, "Wrong password, try again");
                    }
                }));
                return;
            }

            // An unlocked password-protected category offers the admin
            // "library versions" switch; on, it replaces the app list.
            let admin_cat = self
                .active_category
                .as_deref()
                .and_then(|c| cfg.categories.iter().find(|cat| cat.id == c))
                .filter(|cat| !cat.passwords.is_empty());
            if admin_cat.is_some() {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    if toggle_switch(ui, &mut self.show_versions).changed()
                        && self.show_versions
                        && self.versions.is_none()
                    {
                        versions_request = true;
                    }
                    ui.label(egui::RichText::new("Show library versions").strong());
                    ui.label(
                        egui::RichText::new(
                            "— Python packages installed in each environment (NeuNorm, marimo…)",
                        )
                        .weak(),
                    );
                });
                ui.add_space(6.0);
                if self.show_versions {
                    if versions_panel(
                        ui,
                        &cfg.library_versions,
                        self.versions.as_ref(),
                        &mut self.versions_filter,
                    ) {
                        versions_request = true;
                    }
                    return;
                }
                ui.separator();
                ui.add_space(4.0);
            }

            if visible.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(24.0);
                    ui.label(
                        egui::RichText::new("No application matches").weak().italics(),
                    );
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                let group_by_category = self.active_category.is_none();

                // ------------------------------------ recently used ----
                if !recent_rows.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("★ Recently used")
                            .strong()
                            .color(theme::primary_text(ui.visuals())),
                    );
                    ui.add_space(2.0);
                    for &idx in &recent_rows {
                        let app = &cfg.apps[idx];
                        let available =
                            self.available.get(idx).copied().unwrap_or(false);
                        let cooling = self
                            .last_launch
                            .get(&idx)
                            .map(|t| now - t < LAUNCH_COOLDOWN)
                            .unwrap_or(false);
                        let highlighted = self.highlighted == Some(idx);
                        let (resp, clicked) =
                            app_row(ui, app, available, cooling, highlighted);
                        if clicked {
                            launch_request = Some(idx);
                        }
                        if highlighted && self.scroll_to_highlight {
                            resp.scroll_to_me(Some(egui::Align::Center));
                        }
                        if ui.rect_contains_pointer(resp.rect) {
                            self.selected = Some(idx);
                        }
                        ui.add_space(6.0);
                    }
                    ui.separator();
                }

                let mut last_category: Option<&str> = None;
                let mut last_section: Option<&str> = None;
                for idx in visible {
                    let app = &cfg.apps[idx];
                    // Already shown in "Recently used" above — including any
                    // same-name entry the config lists in another category.
                    if recent_names.contains(&app.name.as_str()) {
                        continue;
                    }
                    if group_by_category && last_category != Some(app.category.as_str())
                    {
                        last_category = Some(app.category.as_str());
                        last_section = None;
                        let cat_name = cfg
                            .categories
                            .iter()
                            .find(|c| c.id == app.category)
                            .map(|c| c.name.as_str())
                            .unwrap_or(app.category.as_str());
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(cat_name)
                                .strong()
                                .color(theme::primary_text(ui.visuals())),
                        );
                        ui.add_space(2.0);
                    }
                    if app.section.as_deref() != last_section {
                        last_section = app.section.as_deref();
                        if let Some(section) = last_section {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new(section)
                                    .strong()
                                    .italics()
                                    .color(theme::text_emphasis(ui.visuals())),
                            );
                            ui.add_space(2.0);
                        }
                    }
                    let available = self.available.get(idx).copied().unwrap_or(false);
                    let cooling = self
                        .last_launch
                        .get(&idx)
                        .map(|t| now - t < LAUNCH_COOLDOWN)
                        .unwrap_or(false);
                    let highlighted = self.highlighted == Some(idx);
                    let (resp, clicked) =
                        app_row(ui, app, available, cooling, highlighted);
                    if clicked {
                        launch_request = Some(idx);
                    }
                    if highlighted && self.scroll_to_highlight {
                        resp.scroll_to_me(Some(egui::Align::Center));
                    }
                    if ui.rect_contains_pointer(resp.rect) {
                        self.selected = Some(idx);
                    }
                    ui.add_space(6.0);
                }
            });
        });
        self.scroll_to_highlight = false;

        if versions_request {
            self.versions = Some(VersionsScan::start(&cfg.library_versions));
        }

        if let Some(new_cat) = clicked_category {
            let opens_prompt = new_cat
                .as_deref()
                .map(|c| locked.contains(c))
                .unwrap_or(false);
            self.active_category = new_cat;
            self.password_input.clear();
            self.password_wrong = false;
            self.focus_password = opens_prompt;
        }
        // "Launch anyway" from the pre-launch window: relaunch, skipping the
        // profile-lock check this once.
        let launch_anyway = self.launch_anyway.take();
        if launch_anyway.is_some() {
            launch_request = launch_anyway;
        }
        if let Some(idx) = launch_request {
            let available = self.available.get(idx).copied().unwrap_or(false);
            let cooling = self
                .last_launch
                .get(&idx)
                .map(|t| now - t < LAUNCH_COOLDOWN)
                .unwrap_or(false);
            // A browser tool cannot open while the shared Firefox profile is
            // locked by a session on another machine: hold the launch back
            // and offer the fix instead of spawning a browser that only
            // shows its "already running" dialog.
            let locked_elsewhere = if available
                && !cooling
                && launch_anyway != Some(idx)
                && cfg.apps[idx].involves_browser()
            {
                firefox_remote_lock_report()
            } else {
                None
            };
            if let Some(report) = locked_elsewhere {
                let app = &cfg.apps[idx];
                self.status = Some(Err(format!(
                    "{}: your browser is already running on another machine — \
                     fix it (or launch anyway) in the report window",
                    app.name
                )));
                self.browser_window = Some(BrowserWindow {
                    kind: BrowserWindowKind::LockedBeforeLaunch,
                    app_name: app.name.clone(),
                    report,
                    blocked: Some(idx),
                });
            } else if available && !cooling {
                let app = &cfg.apps[idx];
                self.last_launch.insert(idx, now);
                match app.launch() {
                    Ok((child, log)) => {
                        let verb =
                            if app.url.is_some() { "Opened" } else { "Launched" };
                        self.status = Some(Ok(format!("{verb}: {}", app.name)));
                        self.pending.push(PendingLaunch {
                            name: app.name.clone(),
                            child,
                            started: now,
                            log,
                            browserish: app.involves_browser(),
                        });
                        let entry = self
                            .recent
                            .entries
                            .entry(app.name.clone())
                            .or_insert(RecentEntry { count: 0, last_epoch: 0 });
                        entry.count += 1;
                        entry.last_epoch = epoch_now();
                        save_recent(&self.recent);
                        if let Some(db) = &cfg.usage_db {
                            let _ = record_usage(
                                PathBuf::from(db),
                                app.name.clone(),
                                app.category.clone(),
                                app.url.is_some(),
                            );
                        }
                    }
                    Err(e) => self.status = Some(Err(e)),
                }
            }
        }

        // ----------------------------------------- browser report window ---
        if let Some(win) = &self.browser_window {
            let kind = win.kind;
            let app_name = win.app_name.clone();
            let blocked = win.blocked;
            let mut open = true;
            let mut close = false;
            let (title, intro, advice) = match kind {
                BrowserWindowKind::LockedBeforeLaunch => (
                    "Browser already running elsewhere",
                    format!(
                        "{app_name} needs your web browser, but your Firefox \
                         profile is on shared storage and is locked by a \
                         session on the machine listed below — the browser \
                         would refuse to open on this machine."
                    ),
                    "Click \u{1F527} Fix browser issue to reset the profile \
                     (moves ~/.mozilla aside as ~/.mozilla.bak-<date>; Firefox \
                     then starts fresh here, nothing is killed on the other \
                     machine), then launch again. Or log into that machine \
                     and close the browser there. Launch anyway skips this \
                     check.",
                ),
                BrowserWindowKind::Scan => (
                    "Browser already running elsewhere",
                    format!(
                        "{app_name} could not open the browser: your \
                         Firefox/Chrome profile is on shared storage and is \
                         locked by a session on the machine(s) listed below."
                    ),
                    "Click \u{1F527} Fix browser issue to reset the profile \
                     (moves ~/.mozilla aside as ~/.mozilla.bak-<date>; Firefox \
                     then starts fresh here, nothing is killed on the other \
                     machines), then launch again. Or log into that machine \
                     and close the browser (and any Jupyter) there.",
                ),
            };
            let mut fix = false;
            let mut launch_anyway = false;
            egui::Window::new(title)
                .id(egui::Id::new("browser_window"))
                .open(&mut open)
                .collapsible(false)
                .resizable(true)
                .default_size([680.0, 460.0])
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(&intro).size(16.0));
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(advice)
                            .size(16.0)
                            .color(theme::text_emphasis(ui.visuals())),
                    );
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if fix_browser_button(ui) {
                            fix = true;
                        }
                        if kind == BrowserWindowKind::LockedBeforeLaunch {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("Launch anyway")
                                            .color(theme::TEXT_WHITE),
                                    )
                                    .fill(theme::WARNING)
                                    .corner_radius(6.0)
                                    .min_size(egui::vec2(120.0, 28.0)),
                                )
                                .on_hover_text(
                                    "Launch without fixing — the browser will \
                                     most likely refuse to open",
                                )
                                .clicked()
                            {
                                launch_anyway = true;
                            }
                        }
                        if ui
                            .add(
                                egui::Button::new("Close")
                                    .corner_radius(6.0)
                                    .min_size(egui::vec2(80.0, 28.0)),
                            )
                            .clicked()
                        {
                            close = true;
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                    egui::ScrollArea::both()
                        .id_salt("browser_scan_scroll")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    self.browser_window
                                        .as_ref()
                                        .map(|w| w.report.as_str())
                                        .unwrap_or(""),
                                )
                                .monospace()
                                .size(15.0),
                            );
                        });
                });
            if fix {
                fix_clicked = true;
            }
            if launch_anyway {
                self.launch_anyway = blocked;
                close = true;
            }
            if !open || close {
                self.browser_window = None;
            }
        }
        if fix_clicked {
            self.fix_browser();
        }

        // 1 Hz keeps the config watcher, availability re-check, child reaping
        // and browser-scan polling running without mouse movement; a faster
        // cadence while a cooldown or watched launch is active.
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        if !self.pending.is_empty()
            || self.browser_scan.is_some()
            || self.last_launch.values().any(|t| now - t < LAUNCH_COOLDOWN)
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }
}

fn main() -> eframe::Result<()> {
    let config_path = config_path();
    let title = load_config(&config_path)
        .map(|c| c.title)
        .unwrap_or_else(|_| default_title());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1180.0, 760.0]),
        ..Default::default()
    };
    eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            // Saved light/dark preference, shared by all the VENUS rust
            // tools (dark when none is saved); the search bar has a toggle.
            cc.egui_ctx.set_theme(theme::load());
            cc.egui_ctx.set_zoom_factor(zoom::load());
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(App::new(config_path)))
        }),
    )
}
