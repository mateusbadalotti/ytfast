//! The player along the bottom, in three columns: the playing track with its
//! rating; the transport with the seek bar under it; lyrics, queue and volume.

use egui::{
    Align, Align2, Color32, CursorIcon, Frame, Id, Label, Layout, Rect, RichText, Sense, Stroke,
    Ui, UiBuilder, Vec2, pos2, vec2,
};

use crate::app::{Action, App, Side};
use crate::model::{Loadable, Page, Rating};
use crate::queue::Repeat;
use crate::theme::{self, Icon};
use crate::ui::widgets::{self, format_time, icon_button, icon_button_at};

const ART: f32 = 56.0;
/// Centre lines of the transport's two rows, from the bar's top.
const BUTTONS_Y: f32 = 32.0;
const SEEK_Y: f32 = 68.0;
const PLAY_RADIUS: f32 = 19.0;
const SEEK_MAX_WIDTH: f32 = 600.0;
const TIME_GAP: f32 = 10.0;
const VOLUME_WIDTH: f32 = 80.0;
const LIKE_BUTTON: f32 = 32.0;
/// Kept after the track's text for the like button, gaps included.
const LIKE_ROOM: f32 = 40.0;

pub fn show(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    egui::Panel::bottom("player")
        .exact_size(theme::PLAYER_HEIGHT)
        .resizable(false)
        .frame(Frame::new().fill(theme::SIDEBAR))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            ui.painter().hline(
                rect.x_range(),
                rect.top() + 0.5,
                Stroke::new(1.0, theme::OUTLINE),
            );
            let body = rect.shrink2(vec2(18.0, 0.0));
            let side = (body.width() * 0.3).clamp(240.0, 380.0);
            let left = Rect::from_min_size(body.min, vec2(side, body.height()));
            let right =
                Rect::from_min_max(pos2(body.right() - side.min(300.0), body.top()), body.max);
            let centre = Rect::from_min_max(
                pos2(left.right() + 16.0, body.top()),
                pos2(right.left() - 16.0, body.bottom()),
            );
            track(ui, app, left, actions);
            transport(ui, app, centre, actions);
            let mut extras_ui = ui.new_child(
                UiBuilder::new()
                    .max_rect(right)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            extras(&mut extras_ui, app, actions);
        });
}

fn track(ui: &mut Ui, app: &App, rect: Rect, actions: &mut Vec<Action>) {
    let middle = rect.center().y;
    let Some(track) = app.current() else {
        ui.painter().text(
            pos2(rect.left(), middle),
            Align2::LEFT_CENTER,
            "Nothing playing",
            theme::body(14.0),
            theme::DIM,
        );
        return;
    };
    // The whole column opens the track's menu; the art and the artist links
    // drawn over it keep their own clicks.
    let column_click = ui.interact(rect, Id::new("player-track"), Sense::click());
    column_click.context_menu(|ui| widgets::item_menu(ui, track, app, actions));
    let art = Rect::from_min_size(pos2(rect.left(), middle - ART / 2.0), Vec2::splat(ART));
    widgets::paint_art(ui, art, &track.thumbnails, 6.0);
    let art_click = ui.interact(art, Id::new("player-art"), Sense::click());
    art_click.context_menu(|ui| widgets::item_menu(ui, track, app, actions));
    if art_click
        .on_hover_cursor(CursorIcon::PointingHand)
        .clicked()
        && let Some(id) = track.album.as_ref().and_then(|a| a.id.clone())
    {
        actions.push(Action::Open(Page::Album(id)));
    }
    // Room at the end for the like button, which follows the text.
    let text = Rect::from_min_max(
        pos2(art.right() + 12.0, middle - 21.0),
        pos2(rect.right() - LIKE_ROOM, middle + 21.0),
    );
    let mut column = ui.new_child(
        UiBuilder::new()
            .max_rect(text)
            .layout(Layout::top_down(Align::Min)),
    );
    column.set_clip_rect(text.intersect(ui.clip_rect()));
    column.spacing_mut().item_spacing.y = 2.0;
    column.spacing_mut().interact_size.y = 18.0;
    column.add(
        Label::new(
            RichText::new(&track.title)
                .font(theme::semibold(14.5))
                .color(theme::TEXT),
        )
        .truncate(),
    );
    widgets::artist_links(&mut column, track, 12.5, theme::SECONDARY, actions);

    // One like button beside the track's name, filled once liked.
    if app.signed_in {
        let liked = app.rating_of(&track.id) == Some(Rating::Like);
        let (icon, next, tint, tip) = if liked {
            (Icon::Liked, Rating::None, theme::TEXT, "Remove like")
        } else {
            (Icon::Like, Rating::Like, theme::SECONDARY, "Like")
        };
        let at = Rect::from_center_size(
            pos2(column.min_rect().right() + LIKE_ROOM / 2.0, middle),
            Vec2::splat(LIKE_BUTTON),
        );
        if icon_button_at(ui, at, icon, 17.0, tint, tip).clicked() {
            actions.push(Action::Rate(track.id.clone(), next));
        }
    }
}

fn transport(ui: &mut Ui, app: &App, rect: Rect, actions: &mut Vec<Action>) {
    let x = rect.center().x;
    let y = rect.top() + BUTTONS_Y;
    let on = |active: bool| {
        if active {
            theme::ACCENT_HOVER
        } else {
            theme::SECONDARY
        }
    };
    let at = |dx: f32, size: f32| Rect::from_center_size(pos2(x + dx, y), Vec2::splat(size));
    let queue = &app.settings.queue;

    if icon_button_at(
        ui,
        at(-98.0, 32.0),
        Icon::Shuffle,
        17.0,
        on(queue.shuffle),
        "Shuffle",
    )
    .clicked()
    {
        actions.push(Action::ToggleShuffle);
    }
    if icon_button_at(
        ui,
        at(-52.0, 34.0),
        Icon::Previous,
        20.0,
        theme::TEXT,
        "Previous",
    )
    .clicked()
    {
        actions.push(Action::Previous);
    }
    let play = at(0.0, PLAY_RADIUS * 2.0);
    let response = ui.interact(play, Id::new("play-pause"), Sense::click());
    let fill = if response.hovered() {
        theme::ACCENT_HOVER
    } else {
        theme::ACCENT
    };
    ui.painter().circle_filled(play.center(), PLAY_RADIUS, fill);
    if app.loading && app.playing {
        ui.put(
            Rect::from_center_size(play.center(), Vec2::splat(18.0)),
            egui::Spinner::new().size(18.0).color(Color32::WHITE),
        );
    } else {
        let icon = if app.playing { Icon::Pause } else { Icon::Play };
        // The play triangle's weight sits left of its box.
        let nudge = if app.playing { 0.0 } else { 1.5 };
        icon.image(Color32::WHITE, 18.0).paint_at(
            ui,
            Rect::from_center_size(play.center() + vec2(nudge, 0.0), Vec2::splat(18.0)),
        );
    }
    if response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Play / pause (Space)")
        .clicked()
    {
        actions.push(Action::TogglePlay);
    }
    if icon_button_at(ui, at(52.0, 34.0), Icon::Next, 20.0, theme::TEXT, "Next").clicked() {
        actions.push(Action::Next);
    }
    let (icon, tip) = match queue.repeat {
        Repeat::Off => (Icon::Repeat, "Repeat off"),
        Repeat::All => (Icon::Repeat, "Repeat all"),
        Repeat::One => (Icon::RepeatOne, "Repeat one"),
    };
    if icon_button_at(
        ui,
        at(98.0, 32.0),
        icon,
        17.0,
        on(queue.repeat != Repeat::Off),
        tip,
    )
    .clicked()
    {
        actions.push(Action::CycleRepeat);
    }
    seek(ui, app, rect, actions);
}

/// The seek bar with the time on each side. Dragging previews the spot and
/// seeks on release, so a drag does not seek a hundred times.
fn seek(ui: &mut Ui, app: &App, rect: Rect, actions: &mut Vec<Action>) {
    let length = app.length().unwrap_or(0.0) as f32;
    let width = (rect.width() - 110.0).clamp(160.0, SEEK_MAX_WIDTH);
    let bar = Rect::from_center_size(
        pos2(rect.center().x, rect.top() + SEEK_Y),
        vec2(width, 18.0),
    );
    let preview = Id::new("seek-preview");
    let mut shown = ui
        .data(|d| d.get_temp::<f32>(preview))
        .unwrap_or(app.position() as f32);
    let mut child = ui.new_child(UiBuilder::new().max_rect(bar));
    if length <= 0.0 {
        child.disable();
    }
    let response = widgets::slider(&mut child, &mut shown, 0.0..=length.max(1.0), width, false);
    if response.dragged() {
        ui.data_mut(|d| d.insert_temp(preview, shown));
    }
    if response.drag_stopped() || response.clicked() {
        ui.data_mut(|d| d.remove::<f32>(preview));
        actions.push(Action::Seek(f64::from(shown)));
    }
    let time = |seconds: f32| format_time(f64::from(seconds));
    let painter = ui.painter();
    let font = theme::body(12.0);
    let elapsed = if app.current().is_some() {
        time(shown)
    } else {
        String::new()
    };
    let total = if length > 0.0 {
        time(length)
    } else {
        String::new()
    };
    painter.text(
        bar.left_center() - vec2(TIME_GAP, 0.0),
        Align2::RIGHT_CENTER,
        elapsed,
        font.clone(),
        theme::SECONDARY,
    );
    painter.text(
        bar.right_center() + vec2(TIME_GAP, 0.0),
        Align2::LEFT_CENTER,
        total,
        font,
        theme::SECONDARY,
    );
}

fn extras(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let mut volume = app.settings.volume;
    if widgets::slider(ui, &mut volume, 0.0..=1.0, VOLUME_WIDTH, false).changed() {
        actions.push(Action::Volume(volume));
    }
    let icon = if volume <= 0.0 {
        Icon::Muted
    } else {
        Icon::Volume
    };
    ui.add(icon.image(theme::SECONDARY, 18.0));
    ui.add_space(10.0);
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
    // Greyed out once the track turns out to have none, unless the panel is
    // open, so it can still be closed from here.
    let showing = app.side == Some(Side::Lyrics);
    let none = app
        .current()
        .is_some_and(|t| matches!(app.lyrics.get(&t.id), Some(Loadable::Loaded(None))));
    if none && !showing {
        widgets::icon_unavailable(ui, Icon::Lyrics, 18.0, "No lyrics for this track");
    } else if icon_button(ui, Icon::Lyrics, 18.0, on(showing), "Lyrics").clicked() {
        actions.push(Action::ToggleSide(Side::Lyrics));
    }
}
