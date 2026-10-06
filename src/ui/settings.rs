//! Settings: the account, Home, playback, and the equaliser.
//!
//! One scrolling page of cards. Each setting is a row: what it is and what
//! it does on the left, its control on the right.

use egui::{
    Align, Align2, Frame, Id, Label, Layout, Margin, Rect, RichText, Sense, Stroke, Ui, UiBuilder,
    Vec2, pos2, vec2,
};

use crate::app::{Action, App, HomeSection};
use crate::audio::{EQ_BANDS, EQ_RANGE, EqPreset};
use crate::auth::BROWSERS;
use crate::model::{Kind, Loadable, SearchFilter};
use crate::settings::HomeArtist;
use crate::theme::{self, Icon};
use crate::ui::widgets::{self, pill};

const WIDTH: f32 = 760.0;
const CARD_PADDING: f32 = 20.0;
const ROW_HEIGHT: f32 = 58.0;
const ROW_PADDING: f32 = 12.0;
/// Room the control on the right of a row keeps for itself.
const CONTROL_WIDTH: f32 = 300.0;
const SECTIONS: [(&str, Icon); 4] = [
    ("Account", Icon::User),
    ("Home", Icon::Home),
    ("Playback", Icon::Volume),
    ("Equalizer", Icon::Sliders),
];

/// Where the jump bar asked to scroll, until that section has scrolled there.
fn jump_id() -> Id {
    Id::new("settings-jump")
}

pub fn show(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    ui.add_space(8.0);
    ui.label(
        RichText::new("Settings")
            .font(theme::display(34.0))
            .color(theme::TEXT),
    );
    ui.add_space(14.0);
    jump_bar(ui);

    section(ui, 0, |ui| account(ui, app, actions));
    section(ui, 1, |ui| home(ui, app, actions));
    section(ui, 2, |ui| playback(ui, app, actions));
    section(ui, 3, |ui| equalizer(ui, app, actions));
}

/// A pill per section that scrolls to it.
fn jump_bar(ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for (title, icon) in SECTIONS {
            if pill(ui, Some(icon), title, false).clicked() {
                ui.data_mut(|d| d.insert_temp(jump_id(), title));
            }
        }
    });
}

/// A section: an icon badge and a title over a card.
fn section(ui: &mut Ui, index: usize, body: impl FnOnce(&mut Ui)) {
    let (title, icon) = SECTIONS[index];
    ui.add_space(30.0);
    let header = ui.horizontal(|ui| {
        let (badge, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
        ui.painter().rect_filled(badge, 9.0, theme::RAISED);
        icon.image(theme::ACCENT_HOVER, 16.0).paint_at(
            ui,
            Rect::from_center_size(badge.center(), Vec2::splat(16.0)),
        );
        ui.add_space(4.0);
        ui.label(
            RichText::new(title)
                .font(theme::display(20.0))
                .color(theme::TEXT),
        );
    });
    if ui.data(|d| d.get_temp::<&str>(jump_id())) == Some(title) {
        header.response.scroll_to_me(Some(Align::Min));
        ui.data_mut(|d| d.remove::<&str>(jump_id()));
    }
    ui.add_space(12.0);
    let width = ui.available_width().min(WIDTH);
    Frame::new()
        .fill(theme::SURFACE)
        .stroke(Stroke::new(1.0, theme::OUTLINE))
        .corner_radius(14.0)
        .inner_margin(Margin::symmetric(CARD_PADDING as i8, 6))
        .show(ui, |ui| {
            ui.set_width(width - 2.0 * CARD_PADDING);
            body(ui);
        });
}

/// One setting: its name and what it does, and its control on the right,
/// on the centre line of the text however many lines that wraps to.
fn row(ui: &mut Ui, title: &str, detail: &str, control: impl FnOnce(&mut Ui)) {
    let width = ui.available_width();
    let text_width = width - CONTROL_WIDTH;
    let top = ui.cursor().min;
    let text = ui
        .allocate_ui_with_layout(vec2(text_width, 0.0), Layout::top_down(Align::Min), |ui| {
            ui.set_width(text_width);
            ui.add_space(ROW_PADDING);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(title)
                    .font(theme::semibold(14.5))
                    .color(theme::TEXT),
            );
            if !detail.is_empty() {
                ui.add(
                    Label::new(
                        RichText::new(detail)
                            .font(theme::body(12.5))
                            .color(theme::DIM),
                    )
                    .wrap(),
                );
            }
            ui.add_space(ROW_PADDING);
        })
        .response
        .rect;
    if text.height() < ROW_HEIGHT {
        ui.add_space(ROW_HEIGHT - text.height());
    }
    let height = text.height().max(ROW_HEIGHT);
    let controls =
        Rect::from_min_size(pos2(top.x + text_width, top.y), vec2(CONTROL_WIDTH, height));
    let mut controls = ui.new_child(
        UiBuilder::new()
            .id_salt(("setting", title))
            .max_rect(controls)
            .layout(Layout::right_to_left(Align::Center)),
    );
    control(&mut controls);
}

/// The heading of a group of rows, across the card's whole width.
fn group(ui: &mut Ui, title: &str, detail: &str) {
    ui.scope(|ui| {
        ui.add_space(ROW_PADDING);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            RichText::new(title)
                .font(theme::semibold(14.5))
                .color(theme::TEXT),
        );
        note(ui, detail);
        ui.add_space(ROW_PADDING);
    });
}

/// A hairline between rows of a card.
fn divider(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        Stroke::new(1.0, theme::OUTLINE),
    );
}

fn note(ui: &mut Ui, text: &str) {
    ui.add(
        Label::new(
            RichText::new(text)
                .font(theme::body(12.5))
                .color(theme::DIM),
        )
        .wrap(),
    );
}

const ACCOUNT_ROW: f32 = 72.0;
const ACCOUNT_PHOTO: f32 = 44.0;

fn account(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    if app.signed_in {
        // One fixed-height row, so the photo, the name and the buttons share
        // its centre line.
        let row = vec2(ui.available_width(), ACCOUNT_ROW);
        ui.allocate_ui_with_layout(row, Layout::left_to_right(Align::Center), |ui| {
            ui.set_height(ACCOUNT_ROW);
            widgets::avatar(ui, app.account.as_ref(), ACCOUNT_PHOTO);
            ui.add_space(6.0);
            let name = |account: &crate::model::Account| {
                RichText::new(account.name.clone())
                    .font(theme::semibold(16.0))
                    .color(theme::TEXT)
            };
            match &app.account {
                // YouTube does not always send the email: then the name is
                // the row's only line.
                Some(account) if account.email.is_empty() => _ = ui.label(name(account)),
                Some(account) => {
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(name(account));
                        ui.label(RichText::new(&account.email).color(theme::DIM));
                    });
                }
                None => _ = ui.label("Signed in"),
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pill(ui, Some(Icon::LogOut), "Sign out", false).clicked() {
                    actions.push(Action::SignOut);
                }
                if pill(ui, Some(Icon::Refresh), "Read again", false)
                    .on_hover_text("Read the session from the browser again")
                    .clicked()
                {
                    actions.push(Action::SignIn);
                }
            });
        });
        return;
    }
    row(
        ui,
        "Sign in",
        "With the YouTube session of a browser on this computer. Open music.youtube.com in it and sign in \
         first. Chrome on macOS asks for your Keychain password once; Safari needs Full Disk Access for ytfast.",
        |ui| {
            if app.signing_in {
                ui.label(RichText::new("Reading the session…").color(theme::SECONDARY));
                ui.add(egui::Spinner::new().color(theme::ACCENT));
            } else if pill(ui, Some(Icon::User), "Sign in", true).clicked() {
                actions.push(Action::SignIn);
            }
            egui::ComboBox::from_id_salt("browser")
                .selected_text(capitalize(&app.settings.browser))
                .show_ui(ui, |ui| {
                    for browser in BROWSERS {
                        if ui
                            .selectable_label(app.settings.browser == browser, capitalize(browser))
                            .clicked()
                        {
                            actions.push(Action::SetBrowser(browser.to_string()));
                        }
                    }
                });
        },
    );
    if let Some(error) = &app.sign_in_error {
        ui.label(RichText::new(error).color(theme::ACCENT_HOVER));
        ui.add_space(10.0);
    }
}

fn home(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    home_artists(ui, app, actions);
    divider(ui);
    home_sections(ui, app, actions);
}

const FOUND_SHOWN: usize = 5;
const FOUND_PHOTO: f32 = 36.0;

/// The artists Home follows, and a lookup to add more.
fn home_artists(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    group(
        ui,
        "Your artists",
        "Their sets and new releases get sections of their own on Home.",
    );
    let artists = &app.settings.home_artists;
    if !artists.is_empty() {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for artist in artists {
                let chip = widgets::artist_chip(ui, &artist.name, &artist.thumbnails, false, true);
                if chip.remove {
                    actions.push(Action::RemoveHomeArtist(artist.id.clone()));
                }
            }
        });
        ui.add_space(12.0);
    }
    let field = Id::new("home-artist-query");
    let mut query = ui.data(|d| d.get_temp::<String>(field)).unwrap_or_default();
    Frame::new()
        .fill(theme::RAISED)
        .corner_radius(18.0)
        .inner_margin(Margin::symmetric(14, 8))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(Icon::Search.image(theme::SECONDARY, 15.0));
                let edit = egui::TextEdit::singleline(&mut query)
                    .hint_text("Add an artist — type a name and press Enter")
                    .frame(Frame::NONE)
                    .font(theme::body(14.0))
                    .desired_width(ui.available_width());
                let response = ui.add(edit);
                if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    actions.push(Action::FindArtists(query.clone()));
                }
            });
        });
    ui.data_mut(|d| d.insert_temp(field, query.clone()));
    let asked = app.artist_query.as_str();
    if asked.is_empty() || query.trim() != asked {
        ui.add_space(12.0);
        return;
    }
    ui.add_space(6.0);
    match app
        .searches
        .get(&(asked.to_string(), SearchFilter::Artists))
    {
        Some(Loadable::Loaded(results)) => {
            let found: Vec<_> = results
                .top
                .iter()
                .chain(results.shelves.iter().flat_map(|s| &s.items))
                .filter(|i| i.kind == Kind::Artist)
                .take(FOUND_SHOWN)
                .collect();
            if found.is_empty() {
                note(ui, "No artist by that name.");
            }
            for artist in found {
                let added = artists.iter().any(|a| a.id == artist.id);
                ui.push_id(("found-artist", &artist.id), |ui| {
                    ui.horizontal(|ui| {
                        ui.set_min_height(FOUND_PHOTO + 10.0);
                        let (photo, _) =
                            ui.allocate_exact_size(Vec2::splat(FOUND_PHOTO), Sense::hover());
                        widgets::paint_art(ui, photo, &artist.thumbnails, FOUND_PHOTO / 2.0);
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(
                                RichText::new(&artist.title)
                                    .font(theme::semibold(14.0))
                                    .color(theme::TEXT),
                            );
                            ui.label(
                                RichText::new(&artist.subtitle)
                                    .font(theme::body(12.0))
                                    .color(theme::DIM),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if added {
                                pill(ui, Some(Icon::Check), "Added", false);
                            } else if pill(ui, Some(Icon::Plus), "Add", true).clicked() {
                                actions.push(Action::AddHomeArtist(HomeArtist {
                                    id: artist.id.clone(),
                                    name: artist.title.clone(),
                                    thumbnails: artist.thumbnails.clone(),
                                }));
                            }
                        });
                    });
                });
            }
        }
        Some(Loadable::Failed(error)) => note(ui, &format!("Could not look that up: {error}")),
        _ => _ = ui.add(egui::Spinner::new().color(theme::ACCENT)),
    }
    ui.add_space(12.0);
}

const SECTION_ROW: f32 = 46.0;
/// The up and down buttons at a section row's end.
const ARROWS_WIDTH: f32 = 90.0;

/// Which of Home's sections show, and in what order.
fn home_sections(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    group(
        ui,
        "Sections",
        "Pick what Home shows and in what order. YouTube names its own sections and rotates some of them.",
    );
    let sections = app.home_sections();
    let last = sections.len().saturating_sub(1);
    for (index, section) in sections.iter().enumerate() {
        ui.push_id(("home-section", index), |ui| {
            section_row(ui, app, section, index, last, actions);
        });
    }
    // The feed comes in pages; the app asks for all of them while
    // Settings is open.
    if app.home.get().is_none_or(|f| f.continuation.is_some()) {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(16.0).color(theme::ACCENT));
            note(ui, "Loading the rest of YouTube's sections…");
        });
    }
    let changed = !app.settings.home_order.is_empty() || !app.settings.home_hidden.is_empty();
    if changed {
        ui.add_space(10.0);
        if pill(ui, None, "Reset", false).clicked() {
            actions.push(Action::ResetHome);
        }
    }
    ui.add_space(14.0);
}

fn section_row(
    ui: &mut Ui,
    app: &App,
    section: &HomeSection,
    index: usize,
    last: usize,
    actions: &mut Vec<Action>,
) {
    let title = section.title();
    let shown = app.shows_shelf(title);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, SECTION_ROW), Sense::hover());
    let over = ui.rect_contains_pointer(rect);
    if over {
        ui.painter().rect_filled(rect, 10.0, theme::SURFACE_HOVER);
    }
    let mut inside = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(vec2(10.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    let toggle = widgets::toggle(&mut inside, shown);
    if toggle.clicked() {
        actions.push(Action::ShowShelf(title.to_string(), !shown));
    }
    // Painted on the toggle's centre line.
    let left = toggle.rect.right() + 12.0;
    let clip = Rect::from_min_max(
        pos2(left, rect.top()),
        pos2(rect.right() - ARROWS_WIDTH, rect.bottom()),
    );
    let painter = ui.painter().with_clip_rect(clip);
    let color = if shown { theme::TEXT } else { theme::DIM };
    painter.text(
        pos2(left, toggle.rect.center().y),
        Align2::LEFT_CENTER,
        title,
        theme::medium(14.0),
        color,
    );
    inside.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let tint = |enabled: bool| match (enabled, over) {
            (false, _) => theme::OUTLINE,
            (true, true) => theme::TEXT,
            (true, false) => theme::DIM,
        };
        let down = index < last;
        if widgets::icon_button(ui, Icon::Down, 16.0, tint(down), "Move down").clicked() && down {
            actions.push(Action::MoveShelf {
                title: title.to_string(),
                up: false,
            });
        }
        let up = index > 0;
        if widgets::icon_button(ui, Icon::Up, 16.0, tint(up), "Move up").clicked() && up {
            actions.push(Action::MoveShelf {
                title: title.to_string(),
                up: true,
            });
        }
    });
}

fn playback(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    row(
        ui,
        "Autoplay",
        "Plays similar songs when the queue ends.",
        |ui| {
            if widgets::toggle(ui, app.settings.autoplay).clicked() {
                actions.push(Action::SetAutoplay(!app.settings.autoplay));
            }
        },
    );
    divider(ui);
    row(
        ui,
        "Crossfade",
        "Overlaps the end of a track with the start of the next one.",
        |ui| {
            let mut seconds = app.settings.crossfade;
            let value = if seconds.round() == 0.0 {
                "Off".to_string()
            } else {
                format!("{:.0} s", seconds.round())
            };
            ui.add_sized(
                vec2(36.0, 20.0),
                Label::new(RichText::new(value).color(theme::SECONDARY)),
            );
            if widgets::slider(ui, &mut seconds, 0.0..=12.0, 200.0, false).changed() {
                actions.push(Action::SetCrossfade(seconds.round()));
            }
        },
    );
    if cfg!(target_os = "macos") {
        outputs(ui, app, actions);
    }
}

/// The second output (macOS).
fn outputs(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    divider(ui);
    row(
        ui,
        "Second output",
        "Plays along with the system's output, in step, for two people listening. \
         Pick a device other than the system's own.",
        |ui| {
            let chosen = app.settings.second_output.as_ref();
            let name = match chosen {
                None => "Off".to_string(),
                Some(uid) => app
                    .devices
                    .iter()
                    .find(|(id, _)| id == uid)
                    .map_or_else(|| "Not connected".to_string(), |(_, name)| name.clone()),
            };
            egui::ComboBox::from_id_salt("second-output")
                .selected_text(name)
                .width(220.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(chosen.is_none(), "Off").clicked() {
                        actions.push(Action::SetSecondOutput(None));
                    }
                    for (uid, name) in &app.devices {
                        if ui.selectable_label(chosen == Some(uid), name).clicked() {
                            actions.push(Action::SetSecondOutput(Some(uid.clone())));
                        }
                    }
                });
        },
    );
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

const EQ_COLUMN: f32 = 40.0;
const EQ_HEIGHT: f32 = 150.0;

fn equalizer(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let settings = &app.settings;
    row(
        ui,
        "Equalizer",
        "Nine bands over every track, with presets or a curve of your own.",
        |ui| {
            if widgets::toggle(ui, settings.eq_enabled).clicked() {
                actions.push(Action::SetEqEnabled(!settings.eq_enabled));
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(settings.eq_enabled, |ui| {
                egui::ComboBox::from_id_salt("eq-preset")
                    .selected_text(settings.eq_preset.label())
                    .show_ui(ui, |ui| {
                        for preset in EqPreset::ALL {
                            if ui
                                .selectable_label(settings.eq_preset == preset, preset.label())
                                .clicked()
                            {
                                actions.push(Action::SetEqPreset(preset));
                            }
                        }
                    });
            });
        },
    );
    divider(ui);
    ui.add_space(14.0);
    let gains = settings.eq_gains();
    ui.add_enabled_ui(settings.eq_enabled, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            for (band, hz) in EQ_BANDS.iter().enumerate() {
                let column = vec2(EQ_COLUMN, EQ_HEIGHT + 48.0);
                ui.allocate_ui_with_layout(column, Layout::top_down(Align::Center), |ui| {
                    ui.set_width(EQ_COLUMN);
                    ui.spacing_mut().item_spacing.y = 6.0;
                    let mut gain = gains[band];
                    ui.label(
                        RichText::new(format!("{:+.0}", gain))
                            .font(theme::medium(11.5))
                            .color(theme::TEXT),
                    );
                    let slider =
                        widgets::slider(ui, &mut gain, -EQ_RANGE..=EQ_RANGE, EQ_HEIGHT, true);
                    if slider.on_hover_text(format!("{gain:+.1} dB")).changed() {
                        // Half a decibel is as fine as anyone hears.
                        actions.push(Action::SetEqBand(band, (gain * 2.0).round() / 2.0));
                    }
                    let label = if *hz >= 1000.0 {
                        format!("{}k", hz / 1000.0)
                    } else {
                        format!("{hz}")
                    };
                    ui.label(
                        RichText::new(label)
                            .font(theme::body(11.5))
                            .color(theme::SECONDARY),
                    );
                });
            }
        });
    });
    ui.add_space(10.0);
}
