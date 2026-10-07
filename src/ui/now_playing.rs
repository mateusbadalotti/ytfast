//! The playing track across the window: its art large over a blur of
//! itself, its name, and its lyrics beside it when it has any.

use egui::{
    Align, Color32, Frame, Key, Label, Layout, Rect, RichText, Sense, Ui, UiBuilder, Vec2, pos2,
    vec2,
};

use crate::app::{Action, App};
use crate::model::Loadable;
use crate::theme::{self, Icon};
use crate::ui::{side, widgets};

const MARGIN: f32 = 56.0;
const ART_MAX: f32 = 560.0;
const ART_MIN: f32 = 160.0;
/// Under the art: the title and the artists.
const TEXT_ROOM: f32 = 140.0;
/// The share of the width the art takes when lyrics sit beside it.
const ART_SHARE: f32 = 0.46;
const CLOSE_BUTTON: f32 = 36.0;
const LYRICS_SIZE: f32 = 26.0;
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
            let has_lyrics = matches!(app.lyrics.get(&track.id), Some(Loadable::Loaded(Some(_))));
            let art_side = if has_lyrics {
                Rect::from_min_max(
                    body.min,
                    pos2(body.left() + body.width() * ART_SHARE, body.bottom()),
                )
            } else {
                body
            };

            // The art and the text under it, as one block centred on its side.
            let art = (art_side.width() * 0.85)
                .min(art_side.height() - TEXT_ROOM)
                .clamp(ART_MIN, ART_MAX);
            let block = Rect::from_center_size(art_side.center(), vec2(art, art + TEXT_ROOM));
            let mut column = ui.new_child(
                UiBuilder::new()
                    .id_salt("now-playing-track")
                    .max_rect(block)
                    .layout(Layout::top_down(Align::Min)),
            );
            let (cover, _) = column.allocate_exact_size(Vec2::splat(art), Sense::hover());
            widgets::paint_art(&column, cover, &track.thumbnails, 14.0);
            column.add_space(22.0);
            column.add(
                Label::new(
                    RichText::new(&track.title)
                        .font(theme::display(28.0))
                        .color(theme::TEXT),
                )
                .truncate(),
            );
            column.add_space(4.0);
            widgets::artist_links(&mut column, track, 17.0, theme::SECONDARY, actions);
            if let Some(album) = &track.album {
                column.add(
                    Label::new(
                        RichText::new(&album.name)
                            .font(theme::body(14.0))
                            .color(theme::DIM),
                    )
                    .truncate(),
                );
            }

            if has_lyrics {
                let lyrics_side =
                    Rect::from_min_max(pos2(art_side.right() + MARGIN, body.top()), body.max);
                let mut lyrics = ui.new_child(
                    UiBuilder::new()
                        .id_salt("now-playing-lyrics")
                        .max_rect(lyrics_side)
                        .layout(Layout::top_down(Align::Min)),
                );
                side::lyrics(&mut lyrics, app, actions, LYRICS_SIZE);
            }
        });
}
