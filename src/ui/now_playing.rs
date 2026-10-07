//! The playing track across the window: its art large over a dimmed copy
//! of itself, its name and its subtitle.

use egui::{
    Align, Align2, Color32, FontId, Frame, Id, Key, Label, Layout, Rect, RichText, Sense, Ui,
    UiBuilder, Vec2, pos2, vec2,
};

use crate::app::{Action, App};
use crate::theme::{self, Icon};
use crate::ui::widgets;

const MARGIN: f32 = 56.0;
const ART_MAX: f32 = 390.0;
/// The art's share of the room it could fill: it leaves the view some air.
const ART_SCALE: f32 = 0.7;
const ART_MIN: f32 = 160.0;
const TITLE_SIZE: f32 = 28.0;
const TITLE_GAP: f32 = 22.0;
/// The artists, or the video's details, sit this far above the player.
const DETAILS_GAP: f32 = 16.0;
const CLOSE_BUTTON: f32 = 36.0;
const DIM_BACKDROP: u8 = 150;

pub fn show(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    egui::CentralPanel::default()
        .frame(Frame::new().fill(theme::BG))
        .show(ui, |ui| {
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                actions.push(Action::ToggleNowPlaying);
            }
            let Some(track) = app.current() else {
                actions.push(Action::ToggleNowPlaying);
                return;
            };
            let rect = ui.max_rect();
            widgets::paint_backdrop(ui, rect, &track.thumbnails);
            ui.painter()
                .rect_filled(rect, 0.0, Color32::from_black_alpha(DIM_BACKDROP));
            let lower = Rect::from_min_max(pos2(rect.left(), rect.center().y), rect.max);
            theme::gradient(ui.painter(), lower, Color32::TRANSPARENT, theme::BG);

            let close = Rect::from_min_size(
                pos2(
                    rect.right() - MARGIN / 2.0 - CLOSE_BUTTON,
                    rect.top() + MARGIN / 2.0,
                ),
                Vec2::splat(CLOSE_BUTTON),
            );
            if widgets::icon_button_at(ui, close, Icon::Down, 20.0, theme::TEXT, "Close (Esc)")
                .clicked()
            {
                actions.push(Action::ToggleNowPlaying);
            }

            let body = Rect::from_min_max(
                rect.min + vec2(MARGIN, theme::DRAG_STRIP),
                rect.max - Vec2::splat(MARGIN),
            );
            let font = title_font(ui, &track.title, body.width());
            let text_room = TITLE_GAP + ui.ctx().fonts_mut(|f| f.row_height(&font));
            // The art and the title under it, as one block in the middle.
            let art = ((body.width() * 0.85).min(body.height() - text_room) * ART_SCALE)
                .clamp(ART_MIN, ART_MAX);
            let block = Rect::from_center_size(body.center(), vec2(body.width(), art + text_room));
            let mut column = ui.new_child(
                UiBuilder::new()
                    .id_salt("now-playing-track")
                    .max_rect(block)
                    .layout(Layout::top_down(Align::Center)),
            );
            let (cover, _) = column.allocate_exact_size(Vec2::splat(art), Sense::hover());
            widgets::paint_cover(&column, cover, &track.thumbnails, 14.0);
            column.add_space(TITLE_GAP);
            column.add(
                Label::new(RichText::new(&track.title).font(font).color(theme::TEXT)).extend(),
            );

            // Placed by its bottom edge, which an area knows once it is laid out.
            egui::Area::new(Id::new("now-playing-details"))
                .pivot(Align2::CENTER_BOTTOM)
                .fixed_pos(pos2(body.center().x, rect.bottom() - DETAILS_GAP))
                .show(ui.ctx(), |ui| {
                    ui.set_max_width(body.width());
                    widgets::artist_links_wrapped(ui, track, 17.0, theme::SECONDARY, actions);
                });
        });
}

/// The title's font: full size, or as large as fits `width` on one line.
fn title_font(ui: &Ui, title: &str, width: f32) -> FontId {
    let full = ui
        .painter()
        .layout_no_wrap(title.to_owned(), theme::display(TITLE_SIZE), theme::TEXT)
        .size()
        .x;
    theme::display(TITLE_SIZE * (width / full).min(1.0))
}
