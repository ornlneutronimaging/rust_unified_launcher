//! The portal's tutorial: a window of its own that walks through what the
//! launcher offers (favorites, the Ask box, the instrument selector…), for
//! the users who only ever scroll the list and press Launch.
//!
//! Text only: the launcher is right next to the window, so every page
//! says where to look instead of showing a screenshot.

use eframe::egui;
use egui::RichText;

use crate::theme;

/// One line of a page: a control (or a key) and what it does.
type Item = (&'static str, &'static str);

struct Page {
    title: &'static str,
    /// What the page is about, in one or two sentences.
    intro: &'static str,
    items: &'static [Item],
    /// Good to know, under the items (empty: none).
    tip: &'static str,
}

const PAGES: &[Page] = &[
    Page {
        title: "The portal at a glance",
        intro: "The portal lists every application of the instrument: the Rust tools, the notebooks, the web applications. This tutorial goes through what is on the screen, and through the shortcuts that are easy to miss.",
        items: &[
            ("Green header", "The instrument selector (VENUS / MARS), on the right."),
            ("🔍 Filter box", "Type a few letters to narrow the list."),
            ("💬 Ask box", "Describe what you want to do, in plain words."),
            ("Categories (left)", "The applications by family, your ★ Favorites, or all of them."),
            ("List (centre)", "One row per application, with its ☆ star and its Launch button."),
            ("Preview (right)", "Description and screenshot of the application under the mouse."),
            ("Status bar (bottom)", "What the portal just did: launched, failed, configuration reloaded."),
        ],
        tip: "Three things most people miss: the ☆ star that pins your own applications at the top, the Ask box, and the keyboard (type, ↓, Enter).",
    },
    Page {
        title: "★ Favorites",
        intro: "Star the applications you use the most: they are pinned at the top of the list, so you no longer look for them.",
        items: &[
            ("☆ on a row", "Left of the Launch button. Click it to add the application to your favorites: the star turns into ★."),
            ("★ on a row", "Click it again to remove the application from your favorites."),
            ("★ Favorites block", "At the top of the \"All applications\" view: your starred applications, before everything else."),
            ("★ Favorites (left panel)", "Second entry of the Categories panel, with the number of starred applications: shows them only, grouped by category."),
        ],
        tip: "Your favorites are yours only, and they are still there the next time you open the portal, on any analysis machine. An application that cannot be launched right now (greyed out) can be starred all the same.",
    },
    Page {
        title: "🕓 Recently used",
        intro: "The portal remembers the last applications you launched.",
        items: &[
            ("🕓 Recently used block", "At the top of the \"All applications\" view, under your favorites: the five applications you launched last, the most recent first."),
        ],
        tip: "Nothing to set up: the block fills itself as you launch applications. Like the favorites, it is per user.",
    },
    Page {
        title: "Find an application",
        intro: "Three ways, from the most precise to the most open.",
        items: &[
            ("Categories (left)", "Click a category to list its applications only; \"All applications\" brings everything back."),
            ("🔍 Filter box", "Has the keyboard when the portal opens: just type. It matches the names, the descriptions and the tags."),
            ("↑ / ↓", "Move the selection (green outline) through the filtered list."),
            ("Enter", "Launches the selected application."),
            ("Esc", "Clears the filter; pressed again on an empty filter, closes the portal."),
            ("✖", "At the right of a box once it holds text: clears it."),
        ],
        tip: "Typing three or four letters of the name, then Enter, is the fastest way to launch something you already know.",
    },
    Page {
        title: "💬 Ask: describe what you want to do",
        intro: "When you do not know the name of the tool, write what you want to do in the Ask box, under the filter: for example \"tiff integrated images over a stack of runs\".",
        items: &[
            ("Best matches", "Up to five applications are listed under the box, with their category and the words of your question that matched."),
            ("Hover a match", "Shows it in the preview panel."),
            ("Click a match", "Reveals it in the list."),
            ("Launch / Open", "At the right of each match: starts it from there."),
            ("↑ / ↓ + Enter", "While the Ask box has the keyboard: select a match and launch it."),
            ("Esc", "Clears the question."),
            ("Green button of the Ask box", "When present: opens the advisor tool, which gives the whole workflow, step by step, for your experiment."),
        ],
        tip: "The Ask box is a keyword search, not an AI: use the words of what you want to get (crop, normalize, rotation center, strain…) rather than a long sentence.",
    },
    Page {
        title: "Preview and launch",
        intro: "Every row of the list is one application.",
        items: &[
            ("Hover a row", "The right panel shows its description, its tags, the command it runs and, when there is one, a screenshot."),
            ("Launch", "Starts the application. The button reads \"Launching...\" for a few seconds, so a second click does not start it twice."),
            ("Open", "The same button for a web page: it opens in your web browser."),
            ("Greyed-out row", "The application is not installed on this machine. Hover its button to see what is missing."),
            ("Status bar", "Says whether the application started, or why it did not."),
        ],
        tip: "Some applications take several seconds to show up, the notebooks in particular: wait for the status bar before launching again.",
    },
    Page {
        title: "Instrument selector",
        intro: "The portal serves several beamlines. The buttons at the right of the green header choose which one.",
        items: &[
            ("VENUS / MARS buttons", "The list, the search, the Ask box, your favorites and the recently used applications only show the applications of the selected instrument, plus the ones common to all."),
            ("At start-up", "The portal opens on the instrument of the machine you are logged on."),
        ],
        tip: "Your choice lasts until you close the portal. If an application you expect is missing, check first that the right instrument is selected.",
    },
    Page {
        title: "Applications that open in the web browser",
        intro: "Notebooks, web applications and dashboards run in Firefox. Firefox refuses to start when it is already running for you on another analysis machine (\"Firefox is already running\").",
        items: &[
            ("Before the launch", "When the portal sees that your browser is held by another machine, it holds the launch back and offers \"🔧 Fix browser issue\" or \"Launch anyway\"."),
            ("🔧 Fix browser issue", "Also in the preview panel of every browser application. It sets your browser profile aside so that Firefox starts fresh here; nothing is closed on the other machine."),
        ],
        tip: "After the fix, Firefox starts with a new profile: your bookmarks and saved logins of the previous one are kept in a backup folder (~/.mozilla.bak-<date>), not deleted.",
    },
    Page {
        title: "Text size, theme, locked categories",
        intro: "The remaining buttons.",
        items: &[
            ("🔍+ / 🔍−", "At the right of the filter box: larger or normal text."),
            ("☀ / 🌙", "Next to it: light or dark theme."),
            ("🔒 category", "Reserved to the instrument team: it asks for a password before listing its applications."),
            ("Notice banner", "A message from the instrument team, shown above the filter when there is one. \"Got it\" hides it, for you only."),
            ("Reload config", "Bottom left: reads the list of applications again. Not needed in normal use, the portal does it by itself."),
        ],
        tip: "The text size and the theme are remembered, and they apply to all the VENUS rust tools the next time each one starts.",
    },
];

#[derive(Default)]
pub struct Tutorial {
    open: bool,
    page: usize,
}

impl Tutorial {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    /// Draw the window (when open), in a native window of its own.
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        // No larger than the screen (small laptops, remote desktops), with
        // room for the title bar and the desktop's own bars.
        const MIN: egui::Vec2 = egui::vec2(560.0, 360.0);
        let mut size = egui::vec2(820.0, 640.0);
        if let Some(monitor) = ctx.input(|i| i.viewport().monitor_size) {
            size = size.min(monitor - egui::vec2(40.0, 110.0)).max(MIN);
        }
        let builder = egui::ViewportBuilder::default()
            .with_title("Tutorial — Application portal")
            .with_inner_size(size)
            .with_min_inner_size(MIN);
        let mut close = false;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("portal_tutorial"),
            builder,
            |ctx, class| {
                let embedded = class == egui::ViewportClass::Embedded;
                if embedded {
                    // No native window (single-window backends): a floating
                    // one inside the portal.
                    let mut open = true;
                    egui::Window::new("Tutorial")
                        .open(&mut open)
                        .default_size(size)
                        .show(ctx, |ui| self.page_ui(ui));
                    close = !open;
                    return;
                }
                egui::SidePanel::left("tutorial_pages")
                    .resizable(true)
                    .default_width(230.0)
                    .min_width(170.0)
                    .show(ctx, |ui| self.list(ui));
                // The navigation at the top as well as at the bottom: on a
                // small screen the bottom bar can be out of reach.
                egui::TopBottomPanel::top("tutorial_nav_top").show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| self.prev_next(ui));
                    ui.add_space(4.0);
                });
                egui::TopBottomPanel::bottom("tutorial_nav").show(ctx, |ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| self.prev_next(ui));
                    ui.add_space(4.0);
                });
                egui::CentralPanel::default().show(ctx, |ui| self.page_ui(ui));
                if ctx.input(|i| i.viewport().close_requested()) {
                    close = true;
                }
            },
        );
        if close {
            self.open = false;
        }
    }

    /// Left panel: every page.
    fn list(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(10.0);
                ui.label(theme::section_heading("Tutorial"));
                ui.add_space(6.0);
                for (i, page) in PAGES.iter().enumerate() {
                    if ui
                        .selectable_label(i == self.page, format!("{}. {}", i + 1, page.title))
                        .clicked()
                    {
                        self.page = i;
                    }
                }
            });
    }

    /// Previous / next and the page count (top and bottom bars).
    fn prev_next(&mut self, ui: &mut egui::Ui) {
        if ui
            .add_enabled(self.page > 0, egui::Button::new("◀ Previous"))
            .clicked()
        {
            self.page -= 1;
        }
        if ui
            .add_enabled(self.page + 1 < PAGES.len(), egui::Button::new("Next ▶"))
            .clicked()
        {
            self.page += 1;
        }
        ui.label(RichText::new(format!("page {} of {}", self.page + 1, PAGES.len())).weak());
    }

    fn page_ui(&mut self, ui: &mut egui::Ui) {
        let page = &PAGES[self.page.min(PAGES.len() - 1)];
        let accent = theme::primary_text(ui.visuals());
        egui::ScrollArea::vertical()
            .id_salt(("tutorial_page", self.page))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(10.0);
                ui.label(RichText::new(page.title).strong().size(22.0));
                ui.add_space(8.0);
                ui.label(RichText::new(page.intro).size(14.0));
                ui.add_space(12.0);
                for (what, does) in page.items {
                    egui::Frame::group(ui.style())
                        .corner_radius(6.0)
                        .inner_margin(8.0)
                        .fill(theme::surface_weak(ui.visuals()))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(*what).strong().size(14.0).color(accent));
                            ui.label(*does);
                        });
                    ui.add_space(4.0);
                }
                if !page.tip.is_empty() {
                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Good to know").strong().color(theme::WARNING));
                        ui.label(page.tip);
                    });
                }
                ui.add_space(12.0);
            });
    }
}
