//! Window layout: sidebar, player bar, side panel, the page, toasts.
//!
//! Views read the app and push `Action`s; apart from the search field's text
//! and its focus request, nothing here changes the app's state directly.

mod now_playing;
mod pages;
mod player_bar;
mod settings;
mod side;
mod sidebar;
mod widgets;

use egui::{Align, Frame, Id, Layout, Margin, Rect, RichText, Sense, Ui, ViewportCommand, vec2};

use crate::app::{Action, App};
use crate::model::{Item, Page};
use crate::theme::{self, Icon};

const DIALOG_WIDTH: f32 = 360.0;

pub fn show(app: &mut App, ui: &mut Ui) {
    let mut actions = Vec::new();
    if cfg!(target_os = "macos") {
        drag_strip(ui);
    }
    let full = app.now_playing && app.current().is_some();
    if !full {
        sidebar::show(ui, app, &mut actions);
    }
    player_bar::show(ui, app, &mut actions);
    if full {
        now_playing::show(ui, app, &mut actions);
    } else {
        if let Some(side) = app.side {
            side::show(ui, app, side, &mut actions);
        }
        page(ui, app, &mut actions);
    }
    if let Some(item) = &app.deleting {
        confirm_delete(ui, item, &mut actions);
    }
    toasts(ui, app);
    app.actions.extend(actions);
}

/// The page on screen, under the back and forward buttons and the search field.
fn page(ui: &mut Ui, app: &mut App, actions: &mut Vec<Action>) {
    egui::CentralPanel::default()
        .frame(Frame::new().fill(theme::BG))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            let glow = egui::Rect::from_min_size(
                rect.min,
                vec2(rect.width(), 320.0_f32.min(rect.height())),
            );
            theme::gradient(ui.painter(), glow, theme::GLOW, theme::BG);
            topbar(ui, app, actions);
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
                                Page::Home => pages::home(ui, app, actions),
                                Page::Search => pages::search(ui, app, actions),
                                Page::Library => pages::library(ui, app, actions),
                                Page::Album(id) | Page::Playlist(id) => {
                                    pages::collection(ui, app, id, &page, actions)
                                }
                                Page::Artist(id) => pages::artist(ui, app, id, actions),
                                Page::Browse(target) => pages::feed(ui, app, target, actions),
                                Page::Settings => settings::show(ui, app, actions),
                            }
                        });
                });
        });
}

/// Without a title bar, the empty top of the window moves it, and a double
/// click zooms it. Interacted first, so the buttons and the search field
/// drawn over it keep their clicks.
fn drag_strip(ui: &mut Ui) {
    let area = ui.max_rect();
    let strip = Rect::from_min_size(area.min, vec2(area.width(), theme::DRAG_STRIP));
    let response = ui.interact(strip, Id::new("drag-strip"), Sense::click_and_drag());
    if response.drag_started() {
        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
    }
    if response.double_clicked() {
        let zoomed = ui.input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx()
            .send_viewport_cmd(ViewportCommand::Maximized(!zoomed));
    }
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
                            // On macOS the menu bar's Search takes ⌘F first.
                            let asked = std::mem::take(&mut app.focus_search);
                            if asked
                                || ui.input_mut(|i| {
                                    i.consume_shortcut(&egui::KeyboardShortcut::new(
                                        egui::Modifiers::COMMAND,
                                        egui::Key::F,
                                    ))
                                })
                            {
                                response.request_focus();
                            }
                        });
                    });
            });
        });
}

fn confirm_delete(ui: &mut Ui, item: &Item, actions: &mut Vec<Action>) {
    let mut confirmed = false;
    let modal = egui::Modal::new(Id::new("confirm-delete"))
        .frame(
            Frame::new()
                .fill(theme::RAISED)
                .corner_radius(14.0)
                .inner_margin(22),
        )
        .show(ui.ctx(), |ui| {
            ui.set_width(DIALOG_WIDTH);
            ui.label(
                RichText::new("Delete playlist?")
                    .font(theme::display(20.0))
                    .color(theme::TEXT),
            );
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!(
                    "“{}” will be deleted from your YouTube Music library. This can't be undone.",
                    item.title
                ))
                .color(theme::SECONDARY),
            );
            ui.add_space(18.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                confirmed = widgets::pill(ui, Some(Icon::Trash), "Delete", true).clicked();
                if widgets::pill(ui, None, "Cancel", false).clicked() {
                    actions.push(Action::CancelDelete);
                }
            });
        });
    if confirmed {
        actions.push(Action::ConfirmDelete);
    } else if modal.should_close() {
        // Escape or a click outside.
        actions.push(Action::CancelDelete);
    }
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
