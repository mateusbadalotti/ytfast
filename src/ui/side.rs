//! The right panel: what plays next, or the lyrics of what plays now.

use egui::{Align, Frame, Id, Layout, Margin, RichText, Sense, Ui, vec2};

use crate::app::{Action, App, Side};
use crate::model::{Loadable, LyricsText};
use crate::theme::{self, Icon};
use crate::ui::widgets::{self, Lead, icon_button};

pub fn show(ui: &mut Ui, app: &App, side: Side, actions: &mut Vec<Action>) {
    egui::Panel::right("side")
        .default_size(theme::SIDE_PANEL_WIDTH)
        .min_size(280.0)
        .max_size(520.0)
        .resizable(true)
        .frame(
            Frame::new()
                .fill(theme::SIDEBAR)
                .inner_margin(Margin::symmetric(14, 16)),
        )
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (tab, label) in [(Side::Queue, "Up next"), (Side::Lyrics, "Lyrics")] {
                    let color = if side == tab { theme::TEXT } else { theme::DIM };
                    let text = RichText::new(label).font(theme::display(17.0)).color(color);
                    if ui
                        .add(egui::Label::new(text).sense(Sense::click()))
                        .clicked()
                        && side != tab
                    {
                        actions.push(Action::ToggleSide(tab));
                    }
                    ui.add_space(10.0);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Close, 16.0, theme::SECONDARY, "Close").clicked() {
                        actions.push(Action::ToggleSide(side));
                    }
                });
            });
            ui.add_space(10.0);
            match side {
                Side::Queue => queue(ui, app, actions),
                Side::Lyrics => lyrics(ui, app, actions, SIDE_LYRICS),
            }
        });
}

fn queue(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let queue = &app.settings.queue;
    ui.horizontal(|ui| {
        let mut autoplay = app.settings.autoplay;
        if ui
            .checkbox(&mut autoplay, "Autoplay similar songs")
            .changed()
        {
            actions.push(Action::SetAutoplay(autoplay));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !queue.upcoming().is_empty() && ui.button("Clear").clicked() {
                actions.push(Action::ClearQueue);
            }
        });
    });
    ui.add_space(6.0);
    if queue.tracks.is_empty() {
        widgets::empty(ui, "The queue is empty. Play something.");
        return;
    }
    let current = queue.index;
    egui::ScrollArea::vertical()
        .id_salt("queue")
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (at, item) in queue.tracks.iter().enumerate().skip(current.unwrap_or(0)) {
                if current.is_some_and(|c| at == c + 1) {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new("Next")
                            .font(theme::semibold(12.0))
                            .color(theme::DIM),
                    );
                    ui.add_space(4.0);
                }
                let playing = current == Some(at);
                let row = widgets::track_row(ui, item, Lead::Art, playing, app, actions);
                if row.play && !playing {
                    actions.push(Action::QueueJump(at));
                }
                if row.response.hovered() && !playing {
                    let close = egui::Rect::from_center_size(
                        row.response.rect.right_center() - vec2(24.0, 0.0),
                        egui::Vec2::splat(26.0),
                    );
                    let hit = ui.interact(close, Id::new(("remove", at)), Sense::click());
                    ui.painter()
                        .circle_filled(close.center(), 13.0, theme::OUTLINE);
                    Icon::Close.image(theme::TEXT, 14.0).paint_at(
                        ui,
                        egui::Rect::from_center_size(close.center(), egui::Vec2::splat(14.0)),
                    );
                    if hit.on_hover_text("Remove").clicked() {
                        actions.push(Action::QueueRemove(at));
                    }
                }
            }
        });
}

/// The lyrics' line size in the panel; the line playing is a little larger.
const SIDE_LYRICS: f32 = 18.0;

/// The playing track's lyrics, synced ones following the song, at `size`.
fn lyrics(ui: &mut Ui, app: &App, actions: &mut Vec<Action>, size: f32) {
    let Some(track) = app.current() else {
        widgets::empty(ui, "Play something to see its lyrics.");
        return;
    };
    let lyrics = match app.lyrics.get(&track.id) {
        Some(Loadable::Loaded(Some(lyrics))) => lyrics,
        Some(Loadable::Loaded(None)) => return widgets::empty(ui, "No lyrics for this track."),
        Some(Loadable::Failed(error)) => {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("Lyrics are unavailable right now.").color(theme::SECONDARY),
                );
                ui.label(
                    RichText::new(error)
                        .font(theme::body(12.0))
                        .color(theme::DIM),
                );
                ui.add_space(10.0);
                if widgets::pill(ui, Some(Icon::Refresh), "Try again", false).clicked() {
                    actions.push(Action::RetryLyrics);
                }
            });
            return;
        }
        _ => return widgets::loading(ui),
    };
    let position = app.position() as f32;
    // Each view of the lyrics scrolls and follows the song on its own.
    let view = size as u32;
    egui::ScrollArea::vertical()
        .id_salt(("lyrics", &track.id, view))
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            match &lyrics.text {
                LyricsText::Plain(text) => {
                    ui.label(
                        RichText::new(text)
                            .font(theme::medium(size - 2.0))
                            .color(theme::TEXT),
                    );
                }
                LyricsText::Timed(lines) => {
                    let active = lines.iter().rposition(|l| l.start <= position + 0.15);
                    ui.add_space(40.0);
                    for (i, line) in lines.iter().enumerate() {
                        let current = Some(i) == active;
                        let text = if line.text.is_empty() {
                            "♪"
                        } else {
                            line.text.as_str()
                        };
                        let (font, color) = if current {
                            (theme::display(size + 3.0), theme::TEXT)
                        } else if active.is_some_and(|a| i < a) {
                            (theme::semibold(size), theme::DIM)
                        } else {
                            (theme::semibold(size), theme::SECONDARY)
                        };
                        let response = ui.add(
                            egui::Label::new(RichText::new(text).font(font).color(color))
                                .wrap()
                                .sense(Sense::click()),
                        );
                        if response.clicked() {
                            actions.push(Action::Seek(f64::from(line.start)));
                        }
                        if current {
                            // Follow the song: centre the line once each time it changes.
                            let key = Id::new(("lyrics-line", &track.id, view));
                            if ui.data(|d| d.get_temp::<usize>(key)) != Some(i) {
                                ui.data_mut(|d| d.insert_temp(key, i));
                                response.scroll_to_me(Some(Align::Center));
                            }
                        }
                        ui.add_space(10.0);
                    }
                    ui.add_space(200.0);
                }
            }
            ui.add_space(16.0);
            ui.label(
                RichText::new(format!("Lyrics from {}", lyrics.source))
                    .font(theme::body(12.0))
                    .color(theme::DIM),
            );
        });
}
