//! The player along the bottom: the seek bar on its top edge, the controls,
//! the playing track with its rating, and volume, repeat and shuffle.

use egui::{
    Align, Color32, CursorIcon, Frame, Id, Label, Layout, Rect, RichText, Sense, Ui, UiBuilder,
    Vec2, vec2,
};

use crate::app::{Action, App, Side};
use crate::model::{Page, Rating};
use crate::queue::Repeat;
use crate::theme::{self, Icon};
use crate::ui::widgets::{self, format_time, icon_button};

pub fn show(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    egui::Panel::bottom("player")
        .exact_size(theme::PLAYER_HEIGHT)
        .resizable(false)
        .frame(Frame::new().fill(theme::SIDEBAR))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            seek_bar(ui, app, rect, actions);
            let body = Rect::from_min_max(rect.min + vec2(16.0, 6.0), rect.max - vec2(16.0, 0.0));
            let left = Rect::from_min_size(body.min, vec2(300.0, body.height()));
            let right = Rect::from_min_max(egui::pos2(body.max.x - 330.0, body.min.y), body.max);
            let centre = Rect::from_min_max(
                egui::pos2(left.max.x + 12.0, body.min.y),
                egui::pos2(right.min.x - 12.0, body.max.y),
            );
            controls(
                &mut ui.new_child(
                    UiBuilder::new()
                        .max_rect(left)
                        .layout(Layout::left_to_right(Align::Center)),
                ),
                app,
                actions,
            );
            track(
                &mut ui.new_child(
                    UiBuilder::new()
                        .max_rect(centre)
                        .layout(Layout::left_to_right(Align::Center)),
                ),
                app,
                actions,
            );
            extras(
                &mut ui.new_child(
                    UiBuilder::new()
                        .max_rect(right)
                        .layout(Layout::right_to_left(Align::Center)),
                ),
                app,
                actions,
            );
        });
}

fn seek_bar(ui: &mut Ui, app: &App, panel: Rect, actions: &mut Vec<Action>) {
    let length = app.length().unwrap_or(0.0);
    let zone = Rect::from_min_size(panel.min - vec2(0.0, 4.0), vec2(panel.width(), 12.0));
    let response = ui.interact(zone, Id::new("seek-bar"), Sense::click_and_drag());
    let active = response.hovered() || response.dragged();
    let mut position = app.position();
    if length > 0.0 {
        if let Some(pointer) = response
            .interact_pointer_pos()
            .filter(|_| response.dragged() || response.clicked())
        {
            position =
                f64::from(((pointer.x - zone.left()) / zone.width()).clamp(0.0, 1.0)) * length;
        }
        if response.drag_stopped() || response.clicked() {
            actions.push(Action::Seek(position));
        }
    }
    let fraction = if length > 0.0 {
        (position / length).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let height = if active { 4.0 } else { 2.0 };
    let bar = Rect::from_min_size(panel.min, vec2(panel.width(), height));
    let painter = ui.painter();
    painter.rect_filled(bar, 0.0, theme::OUTLINE);
    let filled = Rect::from_min_size(bar.min, vec2(bar.width() * fraction, height));
    painter.rect_filled(filled, 0.0, theme::ACCENT);
    if active && length > 0.0 {
        painter.circle_filled(egui::pos2(filled.max.x, bar.center().y), 6.0, theme::ACCENT);
        response.clone().on_hover_cursor(CursorIcon::PointingHand);
    }
    if response.dragged() {
        ui.ctx().request_repaint();
    }
}

fn controls(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    if icon_button(ui, Icon::Previous, 20.0, theme::TEXT, "Previous").clicked() {
        actions.push(Action::Previous);
    }
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(46.0), Sense::click());
    let fill = if response.hovered() {
        theme::ACCENT_HOVER
    } else {
        theme::ACCENT
    };
    ui.painter().circle_filled(rect.center(), 23.0, fill);
    if app.loading && app.playing {
        ui.put(
            Rect::from_center_size(rect.center(), Vec2::splat(20.0)),
            egui::Spinner::new().size(20.0).color(Color32::WHITE),
        );
    } else {
        let icon = if app.playing { Icon::Pause } else { Icon::Play };
        let offset = if app.playing { 0.0 } else { 1.5 };
        icon.image(Color32::WHITE, 20.0).paint_at(
            ui,
            Rect::from_center_size(rect.center() + vec2(offset, 0.0), Vec2::splat(20.0)),
        );
    }
    if response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Play / pause (Space)")
        .clicked()
    {
        actions.push(Action::TogglePlay);
    }
    if icon_button(ui, Icon::Next, 20.0, theme::TEXT, "Next").clicked() {
        actions.push(Action::Next);
    }
    ui.add_space(10.0);
    if app.current().is_some() {
        let length = app
            .length()
            .map(format_time)
            .unwrap_or_else(|| "–:––".into());
        let text = format!("{} / {length}", format_time(app.position()));
        ui.label(
            RichText::new(text)
                .font(theme::body(12.5))
                .color(theme::SECONDARY),
        );
    }
}

fn track(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let Some(track) = app.current() else {
        ui.label(RichText::new("Nothing playing").color(theme::DIM));
        return;
    };
    let (art, response) = ui.allocate_exact_size(Vec2::splat(56.0), Sense::click());
    widgets::paint_art(ui, art, &track.thumbnails, 4.0);
    if response.on_hover_cursor(CursorIcon::PointingHand).clicked()
        && let Some(id) = track.album.as_ref().and_then(|a| a.id.clone())
    {
        actions.push(Action::Open(Page::Album(id)));
    }
    ui.add_space(4.0);
    let rating_width = if app.signed_in { 88.0 } else { 0.0 };
    let text_width = (ui.available_width() - rating_width).max(60.0);
    ui.allocate_ui_with_layout(vec2(text_width, 48.0), Layout::top_down(Align::Min), |ui| {
        ui.set_width(text_width);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add_space(4.0);
        ui.add(
            Label::new(
                RichText::new(&track.title)
                    .font(theme::semibold(15.0))
                    .color(theme::TEXT),
            )
            .truncate(),
        );
        widgets::artist_links(ui, track, 13.0, theme::SECONDARY, actions);
    });
    if app.signed_in {
        let rating = app
            .rating
            .as_ref()
            .filter(|(id, _)| *id == track.id)
            .map_or(Rating::None, |(_, r)| *r);
        let (icon, next) = if rating == Rating::Dislike {
            (Icon::Disliked, Rating::None)
        } else {
            (Icon::Dislike, Rating::Dislike)
        };
        if icon_button(ui, icon, 18.0, theme::TEXT, "Dislike").clicked() {
            actions.push(Action::Rate(track.id.clone(), next));
        }
        let (icon, next) = if rating == Rating::Like {
            (Icon::Liked, Rating::None)
        } else {
            (Icon::Like, Rating::Like)
        };
        if icon_button(ui, icon, 18.0, theme::TEXT, "Like").clicked() {
            actions.push(Action::Rate(track.id.clone(), next));
        }
    }
}

fn extras(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let on = |active: bool| {
        if active {
            theme::ACCENT_HOVER
        } else {
            theme::SECONDARY
        }
    };
    if icon_button(
        ui,
        Icon::Queue,
        18.0,
        on(app.side == Some(Side::Queue)),
        "Up next",
    )
    .clicked()
    {
        actions.push(Action::ToggleSide(Side::Queue));
    }
    if icon_button(
        ui,
        Icon::Lyrics,
        18.0,
        on(app.side == Some(Side::Lyrics)),
        "Lyrics",
    )
    .clicked()
    {
        actions.push(Action::ToggleSide(Side::Lyrics));
    }
    let queue = &app.settings.queue;
    if icon_button(ui, Icon::Shuffle, 18.0, on(queue.shuffle), "Shuffle").clicked() {
        actions.push(Action::ToggleShuffle);
    }
    let (icon, tip) = match queue.repeat {
        Repeat::Off => (Icon::Repeat, "Repeat off"),
        Repeat::All => (Icon::Repeat, "Repeat all"),
        Repeat::One => (Icon::RepeatOne, "Repeat one"),
    };
    if icon_button(ui, icon, 18.0, on(queue.repeat != Repeat::Off), tip).clicked() {
        actions.push(Action::CycleRepeat);
    }
    ui.add_space(6.0);
    let mut volume = app.settings.volume;
    let slider = ui.add_sized(
        vec2(96.0, 20.0),
        egui::Slider::new(&mut volume, 0.0..=1.0).show_value(false),
    );
    if slider.changed() {
        actions.push(Action::Volume(volume));
    }
    let icon = if volume <= 0.0 {
        Icon::Muted
    } else {
        Icon::Volume
    };
    ui.add(icon.image(theme::SECONDARY, 18.0));
}
