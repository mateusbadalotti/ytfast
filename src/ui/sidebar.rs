//! The left column: the logo, where to go, and the library's playlists.

use egui::{
    Align, CursorIcon, Frame, Label, Layout, Margin, Rect, RichText, Sense, Ui, Vec2, vec2,
};

use crate::app::{Action, App, LIBRARY_PLAYLISTS};
use crate::model::{Item, Loadable, Page};
use crate::theme::{self, Icon};
use crate::ui::widgets;

const PIN_ICON: f32 = 12.0;
const SIDEBAR_PHOTO: f32 = 26.0;
const PIN_SPACE: f32 = 18.0;

fn nav(ui: &mut Ui, icon: Icon, text: &str, selected: bool) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::click());
    if selected || response.hovered() {
        ui.painter().rect_filled(
            rect,
            8.0,
            if selected {
                theme::SURFACE_HOVER
            } else {
                theme::SURFACE
            },
        );
    }
    let tint = if selected {
        theme::TEXT
    } else {
        theme::SECONDARY
    };
    let icon_rect = Rect::from_center_size(rect.left_center() + vec2(24.0, 0.0), Vec2::splat(20.0));
    icon.image(tint, 20.0).paint_at(ui, icon_rect);
    ui.painter().text(
        rect.left_center() + vec2(48.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        theme::semibold(14.5),
        tint,
    );
    response.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

pub fn show(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    egui::Panel::left("sidebar")
        .exact_size(theme::SIDEBAR_WIDTH)
        .resizable(false)
        .frame(Frame::new().fill(theme::SIDEBAR).inner_margin(Margin {
            left: 12,
            right: 12,
            top: 18 + theme::TRAFFIC_LIGHTS as i8,
            bottom: 18,
        }))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                let (dot, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                ui.painter()
                    .circle_filled(dot.center(), 13.0, theme::ACCENT);
                Icon::Play.image(egui::Color32::WHITE, 12.0).paint_at(
                    ui,
                    Rect::from_center_size(dot.center() + vec2(1.0, 0.0), Vec2::splat(12.0)),
                );
                ui.label(
                    RichText::new("ytfast")
                        .font(theme::display(24.0))
                        .color(theme::TEXT),
                );
            });
            ui.add_space(22.0);
            let page = &app.page;
            if nav(ui, Icon::Home, "Home", *page == Page::Home) {
                actions.push(Action::Open(Page::Home));
            }
            if nav(ui, Icon::Search, "Search", *page == Page::Search) {
                actions.push(Action::Open(Page::Search));
            }
            if nav(ui, Icon::Library, "Library", *page == Page::Library) {
                actions.push(Action::Open(Page::Library));
            }
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);

            let footer = 92.0;
            let list_height = (ui.available_height() - footer).max(0.0);
            ui.allocate_ui(vec2(ui.available_width(), list_height), |ui| {
                ui.set_height(list_height);
                egui::ScrollArea::vertical()
                    .id_salt("sidebar-playlists")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        playlists(ui, app, actions);
                    });
            });

            ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                if nav(ui, Icon::Settings, "Settings", *page == Page::Settings) {
                    actions.push(Action::Open(Page::Settings));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    match (&app.account, app.signed_in) {
                        (Some(account), _) => {
                            widgets::avatar(ui, Some(account), SIDEBAR_PHOTO);
                            ui.add_space(2.0);
                            ui.add(
                                Label::new(RichText::new(&account.name).color(theme::SECONDARY))
                                    .truncate(),
                            );
                        }
                        (None, true) => {
                            _ = ui.label(RichText::new("Signed in").color(theme::SECONDARY))
                        }
                        (None, false) if app.session_ready => {
                            if widgets::pill(ui, Some(Icon::User), "Sign in", true).clicked() {
                                actions.push(Action::Open(Page::Settings));
                            }
                        }
                        // Still reading the stored session: nothing to offer yet.
                        (None, false) => {}
                    }
                });
            });
        });
}

fn playlists(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    if !app.session_ready {
        return;
    }
    if !app.signed_in {
        ui.add_space(8.0);
        ui.label(RichText::new("Sign in to see your playlists here.").color(theme::DIM));
        return;
    }
    match app.library.get(LIBRARY_PLAYLISTS) {
        Some(Loadable::Loaded(items)) => {
            // Pinned first, in the order they were pinned; the rest as the
            // library lists them.
            let pinned = |item: &Item| app.settings.pinned.iter().position(|id| *id == item.id);
            let mut items: Vec<&Item> = items.iter().collect();
            items.sort_by_key(|item| pinned(item).unwrap_or(usize::MAX));
            for item in items {
                let selected = app.page == Page::Playlist(item.id.clone());
                let (rect, response) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                if selected || response.hovered() {
                    ui.painter().rect_filled(
                        rect,
                        6.0,
                        if selected {
                            theme::SURFACE_HOVER
                        } else {
                            theme::SURFACE
                        },
                    );
                }
                let art = Rect::from_min_size(rect.min + vec2(6.0, 5.0), Vec2::splat(36.0));
                widgets::paint_art(ui, art, &item.thumbnails, 4.0);
                let text_rect = Rect::from_min_max(
                    egui::pos2(art.max.x + 10.0, rect.min.y),
                    rect.max - vec2(6.0 + PIN_SPACE, 0.0),
                );
                if pinned(item).is_some() {
                    let pin = Rect::from_center_size(
                        egui::pos2(rect.right() - 6.0 - PIN_SPACE / 2.0, rect.center().y),
                        Vec2::splat(PIN_ICON),
                    );
                    Icon::Pin
                        .image(theme::ACCENT_HOVER, PIN_ICON)
                        .paint_at(ui, pin);
                }
                let mut text = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(text_rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                text.add_space(6.0);
                text.spacing_mut().item_spacing.y = 0.0;
                text.add(
                    Label::new(
                        RichText::new(&item.title)
                            .font(theme::medium(13.5))
                            .color(theme::TEXT),
                    )
                    .truncate(),
                );
                text.add(
                    Label::new(
                        RichText::new(&item.subtitle)
                            .font(theme::body(11.5))
                            .color(theme::DIM),
                    )
                    .truncate(),
                );
                response.context_menu(|ui| widgets::item_menu(ui, item, app, actions));
                if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    actions.push(Action::Open(Page::Playlist(item.id.clone())));
                }
            }
        }
        Some(Loadable::Loading) => {
            ui.add(egui::Spinner::new().color(theme::ACCENT));
        }
        _ => {}
    }
}
