//! The pieces every page is built from: art, cards, track rows, shelves.

use egui::{
    Align, Color32, CursorIcon, Label, Layout, Rect, Response, RichText, Sense, Ui, UiBuilder,
    Vec2, vec2,
};

use crate::app::{Action, App};
use crate::images;
use crate::model::{Item, Kind, Page, Rating, Shelf, Thumbnail};
use crate::theme::{self, Icon};

/// A round icon button that lights up under the pointer.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, tint: Color32, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size + 14.0), Sense::click());
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

/// What can be done with an item, for its right-click menu.
pub fn item_menu(ui: &mut Ui, item: &Item, signed_in: bool, actions: &mut Vec<Action>) {
    let mut add = |ui: &mut Ui, icon: Icon, text: &str, action: Action| {
        if ui
            .add(egui::Button::image_and_text(
                icon.image(theme::TEXT, 16.0),
                text,
            ))
            .clicked()
        {
            actions.push(action);
            ui.close();
        }
    };
    match item.kind {
        Kind::Song | Kind::Video => {
            add(ui, Icon::Play, "Play", Action::OpenItem(item.clone()));
            add(
                ui,
                Icon::PlayNext,
                "Play next",
                Action::PlayNext(item.clone()),
            );
            add(
                ui,
                Icon::AddToQueue,
                "Add to queue",
                Action::AddToQueue(item.clone()),
            );
            add(
                ui,
                Icon::Radio,
                "Start radio",
                Action::StartRadio(item.clone()),
            );
            for artist in &item.artists {
                if let Some(id) = &artist.id {
                    add(
                        ui,
                        Icon::User,
                        &format!("Go to {}", artist.name),
                        Action::Open(Page::Artist(id.clone())),
                    );
                }
            }
            if let Some(id) = item.album.as_ref().and_then(|a| a.id.clone()) {
                add(
                    ui,
                    Icon::Library,
                    "Go to album",
                    Action::Open(Page::Album(id)),
                );
            }
            if signed_in {
                ui.separator();
                add(
                    ui,
                    Icon::Like,
                    "Like",
                    Action::Rate(item.id.clone(), Rating::Like),
                );
                add(
                    ui,
                    Icon::Dislike,
                    "Dislike",
                    Action::Rate(item.id.clone(), Rating::Dislike),
                );
            }
        }
        Kind::Album | Kind::Playlist => {
            add(
                ui,
                Icon::Play,
                "Play",
                Action::PlayCollection {
                    id: item.id.clone(),
                    shuffle: false,
                },
            );
            add(
                ui,
                Icon::Shuffle,
                "Shuffle",
                Action::PlayCollection {
                    id: item.id.clone(),
                    shuffle: true,
                },
            );
            add(ui, Icon::Forward, "Open", Action::OpenItem(item.clone()));
        }
        Kind::Artist => add(ui, Icon::Forward, "Open", Action::OpenItem(item.clone())),
    }
}

fn card_art_height(item: &Item, width: f32) -> f32 {
    if item.kind == Kind::Video {
        width * 9.0 / 16.0
    } else {
        width
    }
}

/// A card: art, with a play button under the pointer, and two lines.
pub fn card(ui: &mut Ui, item: &Item, width: f32, signed_in: bool, actions: &mut Vec<Action>) {
    let height = card_art_height(item, width) + 48.0;
    ui.allocate_ui_with_layout(vec2(width, height), Layout::top_down(Align::Min), |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = 2.0;
        let round = item.kind == Kind::Artist;
        let size = vec2(width, card_art_height(item, width));
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
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
        if response.clicked() {
            let on_button = response
                .interact_pointer_pos()
                .is_some_and(|p| button.contains(p));
            actions.push(match item.kind {
                Kind::Album | Kind::Playlist if on_button && item.play_video_id.is_none() => {
                    Action::PlayCollection {
                        id: item.id.clone(),
                        shuffle: false,
                    }
                }
                _ => Action::OpenItem(item.clone()),
            });
        }
        response.context_menu(|ui| item_menu(ui, item, signed_in, actions));
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

/// A track row: art or number, title and artists, album, length.
pub fn track_row(
    ui: &mut Ui,
    item: &Item,
    lead: Lead,
    playing: bool,
    signed_in: bool,
    actions: &mut Vec<Action>,
) -> Response {
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
    match lead {
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
        }
    }
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
    response.context_menu(|ui| item_menu(ui, item, signed_in, actions));
    response
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

/// A shelf: its title, then a row of cards or columns of track rows.
pub fn shelf(ui: &mut Ui, app: &App, shelf: &Shelf, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        heading(ui, &shelf.title);
        if let Some(more) = &shelf.more {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pill(ui, None, "More", false).clicked() {
                    actions.push(Action::Open(more_page(more)));
                }
            });
        }
    });
    ui.add_space(10.0);
    let playing = app.current().map(|t| t.id.as_str());
    egui::ScrollArea::horizontal()
        .id_salt(("shelf", &shelf.title, shelf.items.len()))
        .show(ui, |ui| {
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
                                    track_row(
                                        ui,
                                        item,
                                        Lead::Art,
                                        playing == Some(item.id.as_str()),
                                        app.signed_in,
                                        actions,
                                    );
                                }
                            },
                        );
                    }
                } else {
                    for item in &shelf.items {
                        card(ui, item, theme::CARD_WIDTH, app.signed_in, actions);
                    }
                }
            });
        });
    ui.add_space(28.0);
}

/// A "More" link to a playlist opens the playlist page; the rest are feeds.
pub fn more_page(more: &crate::model::Browse) -> Page {
    match more.id.strip_prefix("VL") {
        Some(_) => Page::Playlist(more.id.clone()),
        None => Page::Browse(more.clone()),
    }
}

/// Cards wrapped into as many rows as they need.
pub fn card_grid(ui: &mut Ui, items: &[Item], signed_in: bool, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(18.0, 22.0);
        for item in items {
            card(ui, item, theme::CARD_WIDTH, signed_in, actions);
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
