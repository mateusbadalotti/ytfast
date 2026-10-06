//! Window layout: sidebar, player bar, side panel, the page, toasts.
//!
//! Views read the app and push `Action`s; nothing here changes the app's
//! state directly.

mod pages;
mod player_bar;
mod settings;
mod side;
mod sidebar;
pub mod widgets;

use egui::{Align, Frame, Layout, Margin, RichText, Ui, vec2};

use crate::app::{Action, App};
use crate::model::Page;
use crate::theme::{self, Icon};

pub fn show(app: &mut App, ui: &mut Ui) {
    let mut actions = Vec::new();
    sidebar::show(ui, app, &mut actions);
    player_bar::show(ui, app, &mut actions);
    if let Some(side) = app.side {
        side::show(ui, app, side, &mut actions);
    }
    egui::CentralPanel::default()
        .frame(Frame::new().fill(theme::BG))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let glow = egui::Rect::from_min_size(
                rect.min,
                vec2(rect.width(), 320.0_f32.min(rect.height())),
            );
            theme::gradient(ui.painter(), glow, theme::GLOW, theme::BG);
            topbar(ui, app, &mut actions);
            let page = app.page.clone();
            egui::ScrollArea::vertical()
                .id_salt(("page", &page))
                .auto_shrink(false)
                .show(ui, |ui| {
                    Frame::new()
                        .inner_margin(Margin {
                            left: theme::PAGE_MARGIN as i8,
                            right: theme::PAGE_MARGIN as i8,
                            top: 8,
                            bottom: 40,
                        })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            match &page {
                                Page::Home => pages::home(ui, app, &mut actions),
                                Page::Search => pages::search(ui, app, &mut actions),
                                Page::Library => pages::library(ui, app, &mut actions),
                                Page::Album(id) | Page::Playlist(id) => {
                                    pages::collection(ui, app, id, &page, &mut actions)
                                }
                                Page::Artist(id) => pages::artist(ui, app, id, &mut actions),
                                Page::Browse(target) => pages::feed(ui, app, target, &mut actions),
                                Page::Settings => settings::show(ui, app, &mut actions),
                            }
                        });
                });
        });
    toasts(ui, app);
    app.actions.extend(actions);
}

/// Back, forward and the search field.
fn topbar(ui: &mut Ui, app: &mut App, actions: &mut Vec<Action>) {
    Frame::new()
        .inner_margin(Margin {
            left: (theme::PAGE_MARGIN - 8.0) as i8,
            right: theme::PAGE_MARGIN as i8,
            top: 14,
            bottom: 10,
        })
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let tint = |on: bool| if on { theme::TEXT } else { theme::DIM };
                if widgets::icon_button(ui, Icon::Back, 20.0, tint(app.can_go_back()), "Back")
                    .clicked()
                {
                    actions.push(Action::Back);
                }
                if widgets::icon_button(
                    ui,
                    Icon::Forward,
                    20.0,
                    tint(app.can_go_forward()),
                    "Forward",
                )
                .clicked()
                {
                    actions.push(Action::Forward);
                }
                ui.add_space(8.0);
                Frame::new()
                    .fill(theme::SURFACE)
                    .corner_radius(20.0)
                    .inner_margin(Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.add(Icon::Search.image(theme::SECONDARY, 16.0));
                            let edit = egui::TextEdit::singleline(&mut app.search_text)
                                .hint_text("Search songs, albums, artists, playlists")
                                .frame(egui::Frame::NONE)
                                .font(theme::body(15.0))
                                .desired_width(420.0_f32.min(ui.available_width() - 20.0));
                            let response = ui.add(edit);
                            if response.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter))
                            {
                                actions.push(Action::Search(app.search_text.clone()));
                            }
                            if ui.input_mut(|i| {
                                i.consume_shortcut(&egui::KeyboardShortcut::new(
                                    egui::Modifiers::COMMAND,
                                    egui::Key::F,
                                ))
                            }) {
                                response.request_focus();
                            }
                        });
                    });
            });
        });
}

fn toasts(ui: &mut Ui, app: &App) {
    if app.toasts.is_empty() {
        return;
    }
    egui::Area::new(egui::Id::new("toasts"))
        .anchor(
            egui::Align2::CENTER_BOTTOM,
            vec2(0.0, -(theme::PLAYER_HEIGHT + 18.0)),
        )
        .order(egui::Order::Tooltip)
        .show(ui.ctx(), |ui| {
            ui.with_layout(Layout::bottom_up(Align::Center), |ui| {
                for (text, _) in app.toasts.iter().rev().take(3) {
                    Frame::new()
                        .fill(theme::SURFACE_HOVER)
                        .corner_radius(10.0)
                        .inner_margin(Margin::symmetric(16, 10))
                        .show(ui, |ui| ui.label(RichText::new(text).color(theme::TEXT)));
                }
            });
        });
}
