//! The pieces every page is built from: art, cards, track rows, shelves.

use std::ops::RangeInclusive;

use egui::{
    Align, Color32, CursorIcon, Key, Label, Layout, Rect, Response, RichText, Sense, Ui, UiBuilder,
    Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::app::{Action, App};
use crate::images;
use crate::model::{Account, Item, Kind, Loadable, Page, Rating, Shelf, Thumbnail};
use crate::theme::{self, Icon};

/// A round icon button that lights up under the pointer.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, tint: Color32, tooltip: &str) -> Response {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size + ICON_BUTTON_PADDING), Sense::hover());
    icon_button_at(ui, rect, icon, size, tint, tooltip)
}

const ICON_BUTTON_PADDING: f32 = 14.0;

/// An icon button's place with its icon greyed out, for when it has nothing
/// to do; the tooltip says why.
pub fn icon_unavailable(ui: &mut Ui, icon: Icon, size: f32, tooltip: &str) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::splat(size + ICON_BUTTON_PADDING), Sense::hover());
    icon.image(theme::DIM, size)
        .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
    response.on_hover_text(tooltip);
}

/// An icon button in a given place, for rows laid out by hand.
pub fn icon_button_at(
    ui: &mut Ui,
    rect: Rect,
    icon: Icon,
    size: f32,
    tint: Color32,
    tooltip: &str,
) -> Response {
    let response = ui.interact(rect, ui.id().with(("icon-button", tooltip)), Sense::click());
    if response.hovered() {
        ui.painter()
            .circle_filled(rect.center(), rect.width() / 2.0, theme::SURFACE_HOVER);
    }
    icon.image(tint, size)
        .paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(tooltip)
}

/// A thin rounded rail filled up to the value, whose knob shows under the
/// pointer, as in the desktop players. A range across zero fills from zero
/// (the equaliser's bands). Arrow keys move it once it has focus.
pub fn slider(
    ui: &mut Ui,
    value: &mut f32,
    range: RangeInclusive<f32>,
    length: f32,
    vertical: bool,
) -> Response {
    const THICKNESS: f32 = 18.0;
    const RAIL: f32 = 4.0;
    const KNOB: f32 = 6.5;
    let (min, max) = (*range.start(), *range.end());
    let span = (max - min).max(f32::EPSILON);
    let size = if vertical {
        vec2(THICKNESS, length)
    } else {
        vec2(length, THICKNESS)
    };
    let (rect, mut response) = ui.allocate_exact_size(size, Sense::click_and_drag());
    if (response.dragged() || response.clicked())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        let t = if vertical {
            (rect.bottom() - pointer.y) / rect.height()
        } else {
            (pointer.x - rect.left()) / rect.width()
        };
        let new = min + t.clamp(0.0, 1.0) * span;
        if new != *value {
            *value = new;
            response.mark_changed();
        }
    }
    if response.has_focus() {
        let step = span / 20.0;
        let (more, less) = ui.input(|i| {
            (
                i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowDown),
            )
        });
        let delta = if more {
            step
        } else if less {
            -step
        } else {
            0.0
        };
        if delta != 0.0 {
            *value = (*value + delta).clamp(min, max);
            response.mark_changed();
        }
    }
    let enabled = ui.is_enabled();
    let active = enabled && (response.hovered() || response.dragged() || response.has_focus());
    let t = |v: f32| ((v - min) / span).clamp(0.0, 1.0);
    let origin = if min < 0.0 && max > 0.0 { t(0.0) } else { 0.0 };
    let (from, to) = (origin.min(t(*value)), origin.max(t(*value)));
    let centre = rect.center();
    let (rail, fill, knob) = if vertical {
        let rail = Rect::from_center_size(centre, vec2(RAIL, rect.height()));
        let at = |t: f32| rail.bottom() - rail.height() * t;
        let fill = Rect::from_x_y_ranges(rail.x_range(), at(to)..=at(from));
        (rail, fill, pos2(centre.x, at(t(*value))))
    } else {
        let rail = Rect::from_center_size(centre, vec2(rect.width(), RAIL));
        let at = |t: f32| rail.left() + rail.width() * t;
        let fill = Rect::from_x_y_ranges(at(from)..=at(to), rail.y_range());
        (rail, fill, pos2(at(t(*value)), centre.y))
    };
    let painter = ui.painter();
    painter.rect_filled(rail, RAIL / 2.0, theme::RAISED);
    let colour = match (enabled, active) {
        (false, _) => theme::DIM,
        (true, true) => theme::ACCENT,
        (true, false) => theme::TEXT,
    };
    painter.rect_filled(fill, RAIL / 2.0, colour);
    if active {
        painter.circle_filled(knob, KNOB, Color32::WHITE);
    }
    let current = f64::from(*value);
    response.widget_info(|| WidgetInfo {
        value: Some(current),
        ..WidgetInfo::new(WidgetType::Slider)
    });
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A pill button; `primary` is filled with the accent.
pub fn pill(ui: &mut Ui, icon: Option<Icon>, text: &str, primary: bool) -> Response {
    let (fill, color) = if primary {
        (theme::ACCENT, Color32::WHITE)
    } else {
        (theme::SURFACE_HOVER, theme::TEXT)
    };
    let label = RichText::new(text).font(theme::semibold(14.0)).color(color);
    let button = match icon {
        Some(icon) => egui::Button::image_and_text(icon.image(color, 16.0), label),
        None => egui::Button::new(label),
    };
    ui.add(
        button
            .fill(fill)
            .corner_radius(18.0)
            .min_size(vec2(0.0, 36.0)),
    )
    .on_hover_cursor(CursorIcon::PointingHand)
}

/// The part of a still to show in a box of `aspect`: centred, and for
/// YouTube's 4:3 stills, inside their letterbox bars.
fn crop(thumbnail: &Thumbnail, aspect: f32) -> Rect {
    let mut uv = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    let (w, mut h) = (
        thumbnail.width.max(1) as f32,
        thumbnail.height.max(1) as f32,
    );
    if thumbnail.url.contains("i.ytimg.com") && (w * 3.0 - h * 4.0).abs() < 2.0 {
        uv.min.y = 0.125;
        uv.max.y = 0.875;
        h *= 0.75;
    }
    let image = w / h;
    if image > aspect {
        let keep = aspect / image;
        let width = uv.width() * keep;
        uv.min.x = 0.5 - width / 2.0;
        uv.max.x = 0.5 + width / 2.0;
    } else if image < aspect {
        let keep = image / aspect;
        let centre = uv.center().y;
        let height = uv.height() * keep;
        uv.min.y = centre - height / 2.0;
        uv.max.y = centre + height / 2.0;
    }
    uv
}

pub fn paint_art(ui: &Ui, rect: Rect, thumbnails: &[Thumbnail], radius: f32) {
    ui.painter().rect_filled(rect, radius, theme::SURFACE);
    let px = (rect.width().max(rect.height()) * ui.ctx().pixels_per_point()) as u32;
    let Some(thumbnail) = images::pick(thumbnails, px) else {
        return;
    };
    egui::Image::new(images::sized_url(thumbnail, px))
        .uv(crop(thumbnail, rect.width() / rect.height()))
        .corner_radius(radius)
        .paint_at(ui, rect);
}

/// Artists as links, comma-separated, or the subtitle when nothing links.
pub fn artist_links(
    ui: &mut Ui,
    item: &Item,
    size: f32,
    color: Color32,
    actions: &mut Vec<Action>,
) {
    let font = theme::body(size);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        if item.artists.is_empty() {
            ui.add(
                Label::new(
                    RichText::new(&item.subtitle)
                        .font(font.clone())
                        .color(color),
                )
                .truncate(),
            );
            return;
        }
        for (i, artist) in item.artists.iter().enumerate() {
            if i > 0 {
                ui.label(RichText::new(", ").font(font.clone()).color(color));
            }
            let text = RichText::new(&artist.name).font(font.clone()).color(color);
            match &artist.id {
                Some(id) => {
                    if ui.add(egui::Link::new(text)).clicked() {
                        actions.push(Action::Open(Page::Artist(id.clone())));
                    }
                }
                None => _ = ui.label(text),
            }
        }
    });
}

const MENU_WIDTH: f32 = 248.0;
const MENU_ROW: f32 = 34.0;

/// One row of a menu: an icon and a label across the whole width.
fn menu_row(ui: &mut Ui, icon: Icon, text: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(MENU_WIDTH, MENU_ROW), Sense::click());
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 8.0, theme::SURFACE_HOVER);
    }
    let tint = if hovered {
        theme::TEXT
    } else {
        theme::SECONDARY
    };
    icon.image(tint, 16.0).paint_at(
        ui,
        Rect::from_center_size(rect.left_center() + vec2(20.0, 0.0), Vec2::splat(16.0)),
    );
    ui.painter().text(
        rect.left_center() + vec2(40.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        theme::medium(14.0),
        theme::TEXT,
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// The item a menu acts on: its art, title and subtitle.
fn menu_header(ui: &mut Ui, item: &Item) {
    ui.allocate_ui_with_layout(
        vec2(MENU_WIDTH, 52.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            ui.set_width(MENU_WIDTH);
            ui.add_space(6.0);
            let (art, _) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
            let radius = if item.kind == Kind::Artist { 20.0 } else { 4.0 };
            paint_art(ui, art, &item.thumbnails, radius);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.set_width(MENU_WIDTH - 66.0);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.spacing_mut().interact_size.y = 18.0;
                let title = RichText::new(&item.title)
                    .font(theme::semibold(14.0))
                    .color(theme::TEXT);
                ui.add(Label::new(title).truncate());
                let subtitle = RichText::new(&item.subtitle)
                    .font(theme::body(12.0))
                    .color(theme::SECONDARY);
                ui.add(Label::new(subtitle).truncate());
            });
        },
    );
}

fn menu_separator(ui: &mut Ui) {
    ui.add_space(3.0);
    let (rect, _) = ui.allocate_exact_size(vec2(MENU_WIDTH, 1.0), Sense::hover());
    ui.painter().hline(
        rect.shrink2(vec2(8.0, 0.0)).x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, theme::OUTLINE),
    );
    ui.add_space(3.0);
}

/// A menu row that pushes `action` and closes the menu when clicked.
fn menu_action(ui: &mut Ui, icon: Icon, text: &str, action: Action, actions: &mut Vec<Action>) {
    if menu_row(ui, icon, text).clicked() {
        actions.push(action);
        ui.close();
    }
}

/// A menu row that opens `contents` beside it while hovered.
fn menu_submenu(ui: &mut Ui, icon: Icon, text: &str, contents: impl FnOnce(&mut Ui)) {
    let response = menu_row(ui, icon, text);
    let arrow = Rect::from_center_size(
        response.rect.right_center() - vec2(18.0, 0.0),
        Vec2::splat(14.0),
    );
    Icon::Forward
        .image(theme::SECONDARY, 14.0)
        .paint_at(ui, arrow);
    egui::containers::menu::SubMenu::new().show(ui, &response, |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        contents(ui);
    });
}

/// The account's own playlists, the ones a track can be added to.
fn own_playlists(app: &App) -> Vec<&Item> {
    app.library
        .get(crate::app::LIBRARY_PLAYLISTS)
        .and_then(Loadable::get)
        .into_iter()
        .flatten()
        .filter(|item| app.own_playlists.contains(&item.id))
        .collect()
}

/// What can be done with an item, for its right-click menu.
pub fn item_menu(ui: &mut Ui, item: &Item, app: &App, actions: &mut Vec<Action>) {
    let rating = app.rating_of(&item.id);
    if app.signed_in && item.is_playable() && rating.is_none() {
        // Found out while the menu is open; the app asks once.
        actions.push(Action::WantRating(item.id.clone()));
    }
    ui.spacing_mut().item_spacing.y = 0.0;
    menu_header(ui, item);
    menu_separator(ui);
    match item.kind {
        Kind::Song | Kind::Video => {
            menu_action(
                ui,
                Icon::Play,
                "Play",
                Action::OpenItem(item.clone()),
                actions,
            );
            menu_action(
                ui,
                Icon::PlayNext,
                "Play next",
                Action::PlayNext(item.clone()),
                actions,
            );
            menu_action(
                ui,
                Icon::AddToQueue,
                "Add to queue",
                Action::AddToQueue(item.clone()),
                actions,
            );
            menu_action(
                ui,
                Icon::Radio,
                "Start radio",
                Action::StartRadio(item.clone()),
                actions,
            );
            if app.signed_in {
                // One like, filled when the track is liked.
                let (icon, text, next) = if rating == Some(Rating::Like) {
                    (Icon::Liked, "Remove like", Rating::None)
                } else {
                    (Icon::Like, "Like", Rating::Like)
                };
                menu_action(ui, icon, text, Action::Rate(item.id.clone(), next), actions);
                let playlists = own_playlists(app);
                if !playlists.is_empty() {
                    menu_submenu(ui, Icon::AddTo, "Add to playlist", |ui| {
                        for playlist in playlists {
                            let action = Action::AddToPlaylist {
                                playlist: playlist.id.clone(),
                                title: playlist.title.clone(),
                                video: item.id.clone(),
                            };
                            menu_action(ui, Icon::Queue, &playlist.title, action, actions);
                        }
                    });
                }
            }
            menu_action(
                ui,
                Icon::Copy,
                "Copy link",
                Action::CopyLink(item.link()),
                actions,
            );
            let links: Vec<(String, String)> = item
                .artists
                .iter()
                .filter_map(|a| Some((a.id.clone()?, a.name.clone())))
                .collect();
            let album = item.album.as_ref().and_then(|a| a.id.clone());
            if !links.is_empty() || album.is_some() {
                menu_separator(ui);
            }
            for (id, name) in links {
                menu_action(
                    ui,
                    Icon::User,
                    &format!("Go to {name}"),
                    Action::Open(Page::Artist(id)),
                    actions,
                );
            }
            if let Some(id) = album {
                menu_action(
                    ui,
                    Icon::Library,
                    "Go to album",
                    Action::Open(Page::Album(id)),
                    actions,
                );
            }
        }
        Kind::Album | Kind::Playlist => {
            let play = |shuffle| Action::PlayCollection {
                id: item.id.clone(),
                shuffle,
            };
            menu_action(ui, Icon::Play, "Play", play(false), actions);
            menu_action(ui, Icon::Shuffle, "Shuffle", play(true), actions);
            menu_action(
                ui,
                Icon::Forward,
                "Open",
                Action::OpenItem(item.clone()),
                actions,
            );
            menu_action(
                ui,
                Icon::Copy,
                "Copy link",
                Action::CopyLink(item.link()),
                actions,
            );
            let in_library = app
                .library
                .get(crate::app::LIBRARY_PLAYLISTS)
                .and_then(Loadable::get)
                .is_some_and(|items| items.iter().any(|i| i.id == item.id));
            if in_library {
                menu_separator(ui);
                if app.settings.pinned.contains(&item.id) {
                    menu_action(
                        ui,
                        Icon::Unpin,
                        "Unpin from sidebar",
                        Action::Unpin(item.id.clone()),
                        actions,
                    );
                } else {
                    menu_action(
                        ui,
                        Icon::Pin,
                        "Pin to sidebar",
                        Action::Pin(item.id.clone()),
                        actions,
                    );
                }
            }
            if app.own_playlists.contains(&item.id) {
                menu_separator(ui);
                menu_action(
                    ui,
                    Icon::Trash,
                    "Delete playlist",
                    Action::AskDelete(item.clone()),
                    actions,
                );
            }
        }
        Kind::Artist => {
            menu_action(
                ui,
                Icon::Forward,
                "Open",
                Action::OpenItem(item.clone()),
                actions,
            );
            if app.settings.home_artists.iter().any(|a| a.id == item.id) {
                menu_action(
                    ui,
                    Icon::Close,
                    "Remove from Home",
                    Action::RemoveHomeArtist(item.id.clone()),
                    actions,
                );
            } else {
                let artist = crate::settings::HomeArtist {
                    id: item.id.clone(),
                    name: item.title.clone(),
                    thumbnails: item.thumbnails.clone(),
                };
                menu_action(
                    ui,
                    Icon::Home,
                    "Add to Home",
                    Action::AddHomeArtist(artist),
                    actions,
                );
            }
        }
    }
}

/// Tells the app a playable item is under the pointer, and when it is
/// pressed: the first click of a double click, worth resolving at once.
fn report_pointer(ui: &Ui, response: &Response, item: &Item, actions: &mut Vec<Action>) {
    if !response.hovered() {
        return;
    }
    actions.push(Action::Hover(item.id.clone()));
    if ui.input(|i| i.pointer.primary_pressed()) {
        actions.push(Action::Warm(item.id.clone()));
    }
}

/// A card: art, with a play button under the pointer, and two lines.
pub fn card(ui: &mut Ui, item: &Item, width: f32, app: &App, actions: &mut Vec<Action>) {
    // Every card the same square, videos included (their stills are cropped
    // to the centre), so a shelf lines up.
    let height = width + 48.0;
    ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 2.0;
        // The whole card takes the pointer, its two lines included.
        let card = Rect::from_min_size(ui.max_rect().min, vec2(width, height));
        let response = ui.interact(card, ui.auto_id_with("card"), Sense::click());
        let round = item.kind == Kind::Artist;
        let size = Vec2::splat(width);
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let radius = if round { width / 2.0 } else { 6.0 };
        paint_art(ui, rect, &item.thumbnails, radius);
        let playable = item.kind != Kind::Artist;
        let button =
            Rect::from_center_size(rect.right_bottom() - vec2(26.0, 26.0), Vec2::splat(40.0));
        if response.hovered() {
            ui.painter()
                .rect_filled(rect, radius, Color32::from_black_alpha(90));
            if playable {
                let over = response.hover_pos().is_some_and(|p| button.contains(p));
                let fill = if over {
                    theme::ACCENT_HOVER
                } else {
                    theme::ACCENT
                };
                ui.painter().circle_filled(button.center(), 20.0, fill);
                Icon::Play.image(Color32::WHITE, 18.0).paint_at(
                    ui,
                    Rect::from_center_size(button.center(), Vec2::splat(18.0)),
                );
            }
        }
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        if item.is_playable() {
            report_pointer(ui, &response, item, actions);
        }
        // A song plays on a double click or its play button; everything else
        // opens on one click, and an album or playlist plays from its button.
        let single = response.clicked() && !response.double_clicked();
        let on_button = response
            .interact_pointer_pos()
            .is_some_and(|p| button.contains(p));
        if item.is_playable() || item.play_video_id.is_some() {
            if response.double_clicked() || (single && on_button) {
                actions.push(Action::OpenItem(item.clone()));
            }
        } else if single && on_button && item.kind != Kind::Artist {
            actions.push(Action::PlayCollection {
                id: item.id.clone(),
                shuffle: false,
            });
        } else if single {
            actions.push(Action::OpenItem(item.clone()));
        }
        response.context_menu(|ui| item_menu(ui, item, app, actions));
        ui.add_space(6.0);
        let title = RichText::new(&item.title)
            .font(theme::semibold(14.0))
            .color(theme::TEXT);
        ui.add(Label::new(title).truncate());
        let subtitle = RichText::new(&item.subtitle)
            .font(theme::body(12.5))
            .color(theme::SECONDARY);
        ui.add(Label::new(subtitle).truncate());
    });
}

pub fn format_time(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub enum Lead {
    Number(usize),
    Art,
}

/// A drawn track row, and whether the person asked to play it: a double
/// click on the row, or one click on its play icon.
pub struct Row {
    pub response: Response,
    pub play: bool,
}

/// A track row: art or number, title and artists, album, length.
pub fn track_row(
    ui: &mut Ui,
    item: &Item,
    lead: Lead,
    playing: bool,
    app: &App,
    actions: &mut Vec<Action>,
) -> Row {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(vec2(width, theme::ROW_HEIGHT), Sense::click());
    if response.hovered() || playing {
        let fill = if playing {
            theme::SURFACE
        } else {
            theme::SURFACE_HOVER
        };
        ui.painter().rect_filled(rect, 6.0, fill);
    }
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(10.0, 0.0)))
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.set_clip_rect(rect.intersect(ui.clip_rect()));
    let lead_rect = match lead {
        Lead::Number(n) => {
            let (lead_rect, _) = row.allocate_exact_size(vec2(28.0, 40.0), Sense::hover());
            if response.hovered() || playing {
                Icon::Play
                    .image(if playing { theme::ACCENT } else { theme::TEXT }, 14.0)
                    .paint_at(
                        &row,
                        Rect::from_center_size(lead_rect.center(), Vec2::splat(14.0)),
                    );
            } else {
                row.painter().text(
                    lead_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    n.to_string(),
                    theme::body(13.0),
                    theme::DIM,
                );
            }
            lead_rect
        }
        Lead::Art => {
            let (art_rect, _) = row.allocate_exact_size(Vec2::splat(40.0), Sense::hover());
            paint_art(&row, art_rect, &item.thumbnails, 4.0);
            if response.hovered() {
                row.painter()
                    .rect_filled(art_rect, 4.0, Color32::from_black_alpha(120));
                Icon::Play.image(Color32::WHITE, 14.0).paint_at(
                    &row,
                    Rect::from_center_size(art_rect.center(), Vec2::splat(14.0)),
                );
            }
            art_rect
        }
    };
    row.add_space(6.0);
    let remaining = row.available_width();
    let album_width = if remaining > 640.0 {
        remaining * 0.3
    } else {
        0.0
    };
    let time_width = 56.0;
    let main_width = (remaining - album_width - time_width - 12.0).max(80.0);
    row.allocate_ui_with_layout(vec2(main_width, 40.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(main_width);
        ui.spacing_mut().item_spacing.y = 1.0;
        // Lines of text, not a row of buttons: no button-high rows.
        ui.spacing_mut().interact_size.y = 18.0;
        let color = if playing {
            theme::ACCENT_HOVER
        } else {
            theme::TEXT
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if item.explicit {
                ui.label(
                    RichText::new("E")
                        .font(theme::semibold(10.0))
                        .color(theme::SECONDARY)
                        .background_color(theme::OUTLINE),
                );
            }
            ui.add(
                Label::new(
                    RichText::new(&item.title)
                        .font(theme::medium(14.0))
                        .color(color),
                )
                .truncate(),
            );
        });
        artist_links(ui, item, 12.5, theme::SECONDARY, actions);
    });
    if album_width > 0.0 {
        row.allocate_ui_with_layout(
            vec2(album_width, 40.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_width(album_width);
                if let Some(album) = &item.album {
                    let text = RichText::new(&album.name)
                        .font(theme::body(13.0))
                        .color(theme::SECONDARY);
                    match &album.id {
                        Some(id) => {
                            if ui.add(egui::Link::new(text)).clicked() {
                                actions.push(Action::Open(Page::Album(id.clone())));
                            }
                        }
                        None => _ = ui.add(Label::new(text).truncate()),
                    }
                }
            },
        );
    }
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        if let Some(duration) = item.duration {
            ui.label(
                RichText::new(format_time(f64::from(duration)))
                    .font(theme::body(13.0))
                    .color(theme::DIM),
            );
        }
    });
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    report_pointer(ui, &response, item, actions);
    response.context_menu(|ui| item_menu(ui, item, app, actions));
    let on_icon = response
        .interact_pointer_pos()
        .is_some_and(|p| lead_rect.contains(p));
    let play =
        response.double_clicked() || (response.clicked() && !response.double_clicked() && on_icon);
    Row { response, play }
}

/// Fades and slides a page's sections in one after another.
pub fn reveal(ui: &mut Ui, since: f64, index: usize) {
    let now = ui.input(|i| i.time);
    let t = ((now - since - index.min(8) as f64 * 0.06) / 0.35).clamp(0.0, 1.0) as f32;
    let eased = 1.0 - (1.0 - t).powi(3);
    ui.multiply_opacity(eased);
    ui.add_space((1.0 - eased) * 14.0);
    if t < 1.0 {
        ui.ctx().request_repaint();
    }
}

pub fn heading(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::display(22.0))
            .color(theme::TEXT),
    );
}

/// How much of the visible row one arrow click moves.
const SHELF_PAGE: f32 = 0.8;

/// Where a shelf's row stood last frame: its offset and the furthest it goes.
#[derive(Clone, Copy, Default)]
struct ShelfScroll {
    offset: f32,
    max: f32,
}

/// A shelf: its title, then a row of cards or columns of track rows.
///
/// The row scrolls with the trackpad, shift and the wheel, or the arrows in
/// the header; it has no scroll bar, which would sit over the cards' text.
pub fn shelf(ui: &mut Ui, app: &App, shelf: &Shelf, actions: &mut Vec<Action>) {
    shelf_with(ui, app, shelf, actions, |_| {});
}

/// A shelf with something of its own between the heading and the items.
pub fn shelf_with(
    ui: &mut Ui,
    app: &App,
    shelf: &Shelf,
    actions: &mut Vec<Action>,
    below_heading: impl FnOnce(&mut Ui),
) {
    let key = ui.id().with(("shelf", &shelf.title, shelf.items.len()));
    let scroll = ui
        .data(|d| d.get_temp::<ShelfScroll>(key))
        .unwrap_or_default();
    let page = ui.available_width() * SHELF_PAGE;
    let mut step = 0.0;
    // Every shelf has the same arrows: their ids live under the shelf's own.
    ui.push_id(key, |ui| {
        ui.horizontal(|ui| {
            heading(ui, &shelf.title);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if scroll.max > 0.0 {
                    let tint = |on: bool| if on { theme::TEXT } else { theme::DIM };
                    let forward = scroll.offset < scroll.max - 1.0;
                    if icon_button(ui, Icon::Forward, 18.0, tint(forward), "Scroll right").clicked()
                        && forward
                    {
                        step = -page;
                    }
                    let back = scroll.offset > 1.0;
                    if icon_button(ui, Icon::Back, 18.0, tint(back), "Scroll left").clicked()
                        && back
                    {
                        step = page;
                    }
                    ui.add_space(6.0);
                }
                if let Some(more) = &shelf.more
                    && pill(ui, None, "More", false).clicked()
                {
                    actions.push(Action::Open(more_page(more)));
                }
            });
        })
    });
    ui.add_space(10.0);
    below_heading(ui);
    let playing = app.current().map(|t| t.id.as_str());
    let output = egui::ScrollArea::horizontal()
        .id_salt(key)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(ui, |ui| {
            if step != 0.0 {
                ui.scroll_with_delta_animation(
                    vec2(step, 0.0),
                    egui::style::ScrollAnimation::duration(0.35),
                );
            }
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 18.0;
                if shelf.list {
                    for column in shelf.items.chunks(4) {
                        ui.allocate_ui_with_layout(
                            vec2(360.0, 4.0 * theme::ROW_HEIGHT),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(360.0);
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for item in column {
                                    let row = track_row(
                                        ui,
                                        item,
                                        Lead::Art,
                                        playing == Some(item.id.as_str()),
                                        app,
                                        actions,
                                    );
                                    if row.play {
                                        actions.push(Action::OpenItem(item.clone()));
                                    }
                                }
                            },
                        );
                    }
                } else {
                    for item in &shelf.items {
                        card(ui, item, theme::CARD_WIDTH, app, actions);
                    }
                }
            });
        });
    let max = (output.content_size.x - output.inner_rect.width()).max(0.0);
    let offset = output.state.offset.x;
    ui.data_mut(|d| d.insert_temp(key, ShelfScroll { offset, max }));
    ui.add_space(28.0);
}

/// Asked of the photo service, for a crisp circle at any size shown.
const AVATAR_PX: u32 = 128;

/// The account's photo in a circle, or its initial when it has none.
pub fn avatar(ui: &mut Ui, account: Option<&Account>, size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if let Some(url) = account.and_then(|a| a.photo.clone()) {
        let photo = [Thumbnail {
            url,
            width: AVATAR_PX,
            height: AVATAR_PX,
        }];
        return paint_art(ui, rect, &photo, size / 2.0);
    }
    ui.painter()
        .circle_filled(rect.center(), size / 2.0, theme::ACCENT);
    let initial: String = account
        .and_then(|a| a.name.chars().next())
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        theme::semibold(size * 0.46),
        Color32::WHITE,
    );
}

const TOGGLE: Vec2 = vec2(38.0, 22.0);
const KNOB_INSET: f32 = 3.0;

/// An on/off switch.
pub fn toggle(ui: &mut Ui, on: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(TOGGLE, Sense::click());
    let t = ui.ctx().animate_bool_responsive(response.id, on);
    let radius = rect.height() / 2.0;
    let track = theme::OUTLINE.lerp_to_gamma(theme::ACCENT, t);
    ui.painter().rect_filled(rect, radius, track);
    let knob = radius - KNOB_INSET;
    let x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
    ui.painter()
        .circle_filled(pos2(x, rect.center().y), knob, Color32::WHITE);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

const CHIP_HEIGHT: f32 = 32.0;
const CHIP_PHOTO: f32 = 24.0;

/// A chip drawn by `artist_chip`, and whether its ✕ was clicked.
pub struct Chip {
    pub response: Response,
    pub remove: bool,
}

/// A rounded chip with an artist's photo and name. `on` fills it with the
/// accent; `removable` adds a ✕ at its end.
pub fn artist_chip(
    ui: &mut Ui,
    name: &str,
    thumbnails: &[Thumbnail],
    on: bool,
    removable: bool,
) -> Chip {
    let color = if on { Color32::WHITE } else { theme::TEXT };
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_string(), theme::semibold(13.0), color);
    let close = if removable { 22.0 } else { 0.0 };
    let width = 4.0 + CHIP_PHOTO + 8.0 + galley.size().x + 14.0 + close;
    let (rect, response) = ui.allocate_exact_size(vec2(width, CHIP_HEIGHT), Sense::click());
    let fill = if on {
        theme::ACCENT
    } else if response.hovered() {
        theme::SURFACE_HOVER
    } else {
        theme::RAISED
    };
    ui.painter().rect_filled(rect, CHIP_HEIGHT / 2.0, fill);
    let photo = Rect::from_min_size(rect.min + vec2(4.0, 4.0), Vec2::splat(CHIP_PHOTO));
    paint_art(ui, photo, thumbnails, CHIP_PHOTO / 2.0);
    let text = pos2(photo.right() + 8.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text, galley, color);
    let mut remove = false;
    if removable {
        let cross = Rect::from_center_size(
            pos2(rect.right() - 16.0, rect.center().y),
            Vec2::splat(14.0),
        );
        let over = response
            .hover_pos()
            .is_some_and(|p| cross.expand(4.0).contains(p));
        let tint = if over { theme::TEXT } else { theme::DIM };
        Icon::Close.image(tint, 14.0).paint_at(ui, cross);
        remove = response.clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|p| cross.expand(4.0).contains(p));
    }
    Chip {
        response: response.on_hover_cursor(CursorIcon::PointingHand),
        remove,
    }
}

/// A "More" link to a playlist opens the playlist page; the rest are feeds.
pub fn more_page(more: &crate::model::Browse) -> Page {
    match more.id.strip_prefix("VL") {
        Some(_) => Page::Playlist(more.id.clone()),
        None => Page::Browse(more.clone()),
    }
}

/// Cards wrapped into as many rows as they need.
pub fn card_grid(ui: &mut Ui, items: &[Item], app: &App, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(18.0, 22.0);
        for item in items {
            card(ui, item, theme::CARD_WIDTH, app, actions);
        }
    });
}

pub fn loading(ui: &mut Ui) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| ui.add(egui::Spinner::new().size(28.0).color(theme::ACCENT)));
}

/// An error with a retry button.
pub fn failed(ui: &mut Ui, error: &str, retry: Page, actions: &mut Vec<Action>) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new("Something went wrong").font(theme::display(20.0)));
        ui.add_space(6.0);
        ui.label(RichText::new(error).color(theme::SECONDARY));
        ui.add_space(12.0);
        if pill(ui, Some(Icon::Refresh), "Try again", true).clicked() {
            actions.push(Action::Retry(retry));
        }
    });
}

pub fn empty(ui: &mut Ui, text: &str) {
    ui.add_space(40.0);
    ui.vertical_centered(|ui| ui.label(RichText::new(text).color(theme::SECONDARY)));
}
