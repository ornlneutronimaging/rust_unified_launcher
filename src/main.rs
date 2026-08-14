//! Unified launcher for all neutron imaging entry points (Rust GUIs, Jupyter
//! portals, marimo portals, Python applications).
//!
//! Everything the launcher shows comes from `applications.toml` next to this
//! repository — adding, removing or editing an application never requires a
//! recompile. Each entry is a plain argv command (usually one of the existing
//! `menu/start_*` or repo `launch_*.sh` scripts), so the launch logic itself
//! stays where it always was.

mod theme;

use eframe::egui;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

const DEFAULT_CONFIG: &str =
    "/SNS/VENUS/shared/software/git/rust_unified_launcher/applications.toml";
const LOGO_BYTES: &[u8] = include_bytes!("../logos/ImagingLogo.png");
const LOGO_MAX_HEIGHT: f32 = 56.0;
const PREVIEW_PANEL_WIDTH: f32 = 340.0;
const CATEGORY_PANEL_WIDTH: f32 = 210.0;
/// Seconds during which an app's Launch button stays disabled after a click.
const LAUNCH_COOLDOWN: f64 = 5.0;

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
    command: Vec<String>,
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
        !self.command.is_empty()
            && self.checked_path().map(|p| p.exists()).unwrap_or(true)
    }

    fn launch(&self) -> Result<String, String> {
        if self.command.is_empty() {
            return Err(format!("{}: no command configured", self.name));
        }
        if self.clear_fontconfig {
            if let Some(home) = std::env::var_os("HOME") {
                let _ = std::fs::remove_dir_all(
                    Path::new(&home).join(".cache/fontconfig"),
                );
            }
        }

        let mut argv: Vec<String> = self.command.clone();
        if self.in_terminal {
            if let Some((term, term_args)) = find_terminal() {
                let mut wrapped = vec![term];
                wrapped.extend(term_args);
                if self.hold_terminal {
                    let joined = self
                        .command
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

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(workdir)
            // Detach from the launcher's terminal so the app keeps running
            // (and stays quiet) after the portal exits.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Put the app in its own session so closing the portal (or its
        // terminal) never delivers SIGHUP/SIGINT/SIGTERM to launched apps.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn()
            .map(|_| format!("Launched: {}", self.name))
            .map_err(|e| format!("Cannot launch {}: {e}", argv[0]))
    }

    fn matches(&self, needle: &str) -> bool {
        if needle.is_empty() {
            return true;
        }
        let needle = needle.to_lowercase();
        self.name.to_lowercase().contains(&needle)
            || self.description.to_lowercase().contains(&needle)
            || self
                .tags
                .iter()
                .any(|t| t.to_lowercase().contains(&needle))
    }
}

/// Single-quote a string for safe interpolation into a `bash -c` command.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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

struct App {
    config_path: PathBuf,
    config: Result<Config, String>,
    logo: Option<egui::TextureHandle>,
    available: Vec<bool>,
    previews: HashMap<usize, Preview>,
    /// Index into apps of the entry shown in the preview panel (last hovered).
    selected: Option<usize>,
    /// Category id filter; `None` shows every category.
    active_category: Option<String>,
    search: String,
    /// Give the search bar keyboard focus on the next frame (set at startup).
    focus_search: bool,
    status: Option<Result<String, String>>,
    /// Per-app time (egui clock) of the last launch, for the cooldown.
    last_launch: HashMap<usize, f64>,
}

impl App {
    fn new(config_path: PathBuf) -> Self {
        let config = load_config(&config_path);
        let available = match &config {
            Ok(cfg) => cfg.apps.iter().map(|a| a.available()).collect(),
            Err(_) => Vec::new(),
        };
        Self {
            config_path,
            config,
            logo: None,
            available,
            previews: HashMap::new(),
            selected: None,
            active_category: None,
            search: String::new(),
            focus_search: true,
            status: None,
            last_launch: HashMap::new(),
        }
    }

    fn reload(&mut self) {
        let selected_name = self.selected_app_name();
        *self = App {
            logo: self.logo.take(),
            ..App::new(self.config_path.clone())
        };
        // Keep the preview on the same app when it still exists.
        if let (Ok(cfg), Some(name)) = (&self.config, selected_name) {
            self.selected = cfg.apps.iter().position(|a| a.name == name);
        }
        self.status = Some(Ok("Configuration reloaded".to_owned()));
    }

    fn selected_app_name(&self) -> Option<String> {
        match (&self.config, self.selected) {
            (Ok(cfg), Some(idx)) => cfg.apps.get(idx).map(|a| a.name.clone()),
            _ => None,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.logo.is_none() {
            self.logo = load_texture(ctx, "imaging_logo", LOGO_BYTES);
        }
        let now = ctx.input(|i| i.time);

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
                    self.reload();
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
            });
            ui.add_space(4.0);
        });

        // A config error replaces the whole body.
        let cfg = match &self.config {
            Ok(cfg) => cfg,
            Err(msg) => {
                let msg = msg.clone();
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        ui.colored_label(theme::DANGER, "Cannot load the application list");
                        ui.add_space(8.0);
                        ui.label(msg);
                        ui.add_space(8.0);
                        ui.label("Fix the file, then press \"Reload config\" below.");
                    });
                });
                return;
            }
        };

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
                                    .hint_text(
                                        "Type to filter by name, description or tag…",
                                    )
                                    .desired_width(ui.available_width()),
                            );
                            if self.focus_search {
                                response.request_focus();
                                self.focus_search = false;
                            }
                            if response.has_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                self.search.clear();
                            }
                        },
                    );
                });
            });

        // ---------------------------------------------- category sidebar ---
        let mut clicked_category: Option<Option<String>> = None;
        egui::SidePanel::left("categories")
            .exact_width(CATEGORY_PANEL_WIDTH)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_space(12.0);
                ui.label(theme::section_heading("Categories"));
                ui.add_space(6.0);
                let total = cfg.apps.len();
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
                    if ui
                        .selectable_label(is_active, format!("{}  ({count})", cat.name))
                        .clicked()
                    {
                        clicked_category = Some(Some(cat.id.clone()));
                    }
                }
            });

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
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new(&app.name).strong().size(16.0));
                });
                ui.add_space(8.0);
                if !self.previews.contains_key(&idx) {
                    let preview = load_preview(ctx, app, idx);
                    self.previews.insert(idx, preview);
                }
                match self.previews.get(&idx) {
                    Some(Preview::Loaded(tex)) => {
                        ui.vertical_centered(|ui| {
                            ui.add(
                                egui::Image::from_texture(tex)
                                    .max_width(ui.available_width())
                                    .max_height(ui.available_height() - 12.0),
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

        // ------------------------------------------------- application list
        let mut launch_request: Option<usize> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);

            let visible: Vec<usize> = cfg
                .apps
                .iter()
                .enumerate()
                .filter(|(_, a)| {
                    self.active_category
                        .as_deref()
                        .map(|c| a.category == c)
                        .unwrap_or(true)
                        && a.matches(&self.search)
                })
                .map(|(i, _)| i)
                .collect();

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
                let mut last_category: Option<&str> = None;
                let mut last_section: Option<&str> = None;
                for idx in visible {
                    let app = &cfg.apps[idx];
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
                    let group = egui::Frame::group(ui.style())
                        .corner_radius(6.0)
                        .inner_margin(10.0)
                        .fill(theme::surface_weak(ui.visuals()))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    ui.set_width(ui.available_width() - 120.0);
                                    ui.label(
                                        egui::RichText::new(&app.name)
                                            .strong()
                                            .size(16.0),
                                    );
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
                                            let hover = app
                                                .checked_path()
                                                .map(|p| p.display().to_string())
                                                .unwrap_or_else(|| {
                                                    app.command.join(" ")
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
                                                launch_request = Some(idx);
                                            }
                                        });
                                    },
                                );
                            });
                        });
                    if ui.rect_contains_pointer(group.response.rect) {
                        self.selected = Some(idx);
                    }
                    ui.add_space(6.0);
                }
            });
        });

        if let Some(new_cat) = clicked_category {
            self.active_category = new_cat;
        }
        if let Some(idx) = launch_request {
            self.last_launch.insert(idx, now);
            self.status = Some(cfg.apps[idx].launch());
            // Keep repainting so the cooldown expires without mouse movement.
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
        if self.last_launch.values().any(|t| now - t < LAUNCH_COOLDOWN) {
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
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(App::new(config_path)))
        }),
    )
}
