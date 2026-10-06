//! The pages of the central panel.

use std::hash::Hash;

use egui::{
    Align, Align2, Color32, CursorIcon, Id, Label, Layout, Rect, RichText, Sense, Ui, Vec2, vec2,
};

use crate::app::{
    Action, App, HOME_PLAYLISTS, HOME_SETS, HomeSection, LIBRARY_ALBUMS, LIBRARY_ARTISTS,
    LIBRARY_PLAYLISTS,
};
use crate::model::{Browse, Collection, Item, Kind, Loadable, Page, SearchFilter, Shelf};
use crate::theme::{self, Icon};
use crate::ui::widgets::{self, Lead, card_grid, failed, loading, pill, reveal};

/// When content under `key` was first drawn, for its reveal.
fn since(ui: &Ui, key: impl Hash + std::fmt::Debug) -> f64 {
    let now = ui.input(|i| i.time);
    ui.data_mut(|d| *d.get_temp_mut_or_insert_with(Id::new(("reveal", key)), || now))
}

fn title(ui: &mut Ui, text: &str, size: f32) {
    ui.add(
        Label::new(
            RichText::new(text)
                .font(theme::display(size))
                .color(theme::TEXT),
        )
        .wrap(),
    );
}

const RECENT_WIDTH: f32 = 640.0;
const RECENT_ROW: f32 = 46.0;

/// The last searches, newest first; a click runs one again.
fn recent_searches(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let recent = &app.settings.recent_searches;
    if recent.is_empty() {
        return;
    }
    let width = ui.available_width().min(RECENT_WIDTH);
    ui.add_space(30.0);
    ui.allocate_ui_with_layout(
        vec2(width, 30.0),
        Layout::left_to_right(Align::Center),
        |ui| {
            widgets::heading(ui, "Recent searches");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let clear = egui::Button::new(
                    RichText::new("Clear all")
                        .font(theme::semibold(13.0))
                        .color(theme::SECONDARY),
                )
                .frame_when_inactive(false)
                .corner_radius(14.0);
                if ui
                    .add(clear)
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .clicked()
                {
                    actions.push(Action::ClearSearches);
                }
            });
        },
    );
    ui.add_space(10.0);
    let since = since(ui, "recent-searches");
    ui.spacing_mut().item_spacing.y = 2.0;
    for (index, query) in recent.iter().enumerate() {
        ui.scope(|ui| {
            reveal(ui, since, index);
            recent_search(ui, query, width, actions);
        });
    }
}

fn recent_search(ui: &mut Ui, query: &str, width: f32, actions: &mut Vec<Action>) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, RECENT_ROW), Sense::click());
    // The pointer may be on the remove button, which takes the row's hover.
    let over = ui.rect_contains_pointer(rect);
    if over {
        ui.painter().rect_filled(rect, 10.0, theme::SURFACE_HOVER);
    }
    let badge = Rect::from_center_size(rect.left_center() + vec2(24.0, 0.0), Vec2::splat(32.0));
    ui.painter()
        .circle_filled(badge.center(), 16.0, theme::SURFACE);
    Icon::History.image(theme::SECONDARY, 16.0).paint_at(
        ui,
        Rect::from_center_size(badge.center(), Vec2::splat(16.0)),
    );
    let text = Rect::from_min_max(rect.min + vec2(52.0, 0.0), rect.max - vec2(52.0, 0.0));
    ui.painter().with_clip_rect(text).text(
        text.left_center(),
        Align2::LEFT_CENTER,
        query,
        theme::medium(15.0),
        theme::TEXT,
    );
    if over {
        let remove =
            Rect::from_center_size(rect.right_center() - vec2(24.0, 0.0), Vec2::splat(30.0));
        if widgets::icon_button_at(ui, remove, Icon::Close, 16.0, theme::SECONDARY, "Remove")
            .clicked()
        {
            actions.push(Action::ForgetSearch(query.to_string()));
            return;
        }
    }
    if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        actions.push(Action::Search(query.to_string()));
    }
}

/// A spinner that asks for more once it scrolls into view.
fn load_more(ui: &mut Ui, action: Action, actions: &mut Vec<Action>) {
    ui.add_space(12.0);
    let response = ui
        .vertical_centered(|ui| ui.add(egui::Spinner::new().color(theme::ACCENT)))
        .inner;
    if ui.is_rect_visible(response.rect) {
        actions.push(action);
    }
}

/// Home: YouTube's shelves and the app's own, in the order and with the
/// ones picked in Settings. The app's own draw without waiting for the feed.
pub fn home(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    ui.add_space(8.0);
    let sections: Vec<HomeSection> = app
        .home_sections()
        .into_iter()
        .filter(|s| app.shows_shelf(s.title()))
        .collect();
    let feed = app.home.get();
    let key = feed
        .and_then(|f| f.shelves.first())
        .map(|s| s.title.clone());
    let since = since(ui, ("home", key));
    for (i, section) in sections.iter().enumerate() {
        ui.scope(|ui| {
            reveal(ui, since, i);
            match section {
                HomeSection::Feed(shelf) => widgets::shelf(ui, app, shelf, actions),
                HomeSection::Playlists => playlists_shelf(ui, app, actions),
                HomeSection::Sets => sets_shelf(ui, app, actions),
                HomeSection::New => new_releases(ui, app, actions),
            }
        });
    }
    match &app.home {
        Loadable::Loaded(feed) if feed.continuation.is_some() => {
            load_more(ui, Action::LoadMoreHome, actions)
        }
        Loadable::Loaded(_) => {}
        Loadable::Failed(error) => failed(ui, error, Page::Home, actions),
        _ => loading(ui),
    }
}

fn playlists_shelf(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let Some(items) = app.library.get(LIBRARY_PLAYLISTS).and_then(Loadable::get) else {
        return;
    };
    if items.is_empty() {
        return;
    }
    let shelf = Shelf {
        title: HOME_PLAYLISTS.to_string(),
        items: items.clone(),
        list: false,
        more: None,
    };
    widgets::shelf(ui, app, &shelf, actions);
}

/// The picked artists as chips, and the sets of the one chosen.
fn sets_shelf(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let Some(chosen) = app.sets_artist_now() else {
        return;
    };
    let sets = app.artist_sets.get(&chosen.id);
    let shelf = Shelf {
        title: HOME_SETS.to_string(),
        items: sets.and_then(Loadable::get).cloned().unwrap_or_default(),
        list: false,
        more: None,
    };
    let mut picked = None;
    widgets::shelf_with(ui, app, &shelf, actions, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            for artist in &app.settings.home_artists {
                let on = artist.id == chosen.id;
                let chip = widgets::artist_chip(ui, &artist.name, &artist.thumbnails, on, false);
                if chip.response.clicked() && !on {
                    picked = Some(artist.id.clone());
                }
            }
        });
        ui.add_space(14.0);
        let status = match sets {
            Some(Loadable::Loaded(items)) if items.is_empty() => {
                format!("No sets of half an hour or more found for {}.", chosen.name)
            }
            Some(Loadable::Failed(error)) => format!("Could not look up sets: {error}"),
            Some(Loadable::Loaded(_)) => return,
            _ => {
                ui.add(egui::Spinner::new().size(20.0).color(theme::ACCENT));
                return;
            }
        };
        ui.label(RichText::new(status).color(theme::SECONDARY));
    });
    if let Some(id) = picked {
        actions.push(Action::PickSetsArtist(id));
    }
}

/// A shelf per picked artist: their latest singles and albums, then their
/// top songs.
fn new_releases(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    for artist in &app.settings.home_artists {
        let Some(page) = app.artists.get(&artist.id).and_then(Loadable::get) else {
            continue;
        };
        let take = |title: &str, count: usize| {
            page.shelves
                .iter()
                .find(|s| s.title.starts_with(title))
                .into_iter()
                .flat_map(move |s| s.items.iter().take(count).cloned())
        };
        let items: Vec<Item> = take("Singles", NEW_SINGLES)
            .chain(take("Albums", NEW_ALBUMS))
            .chain(take("Top songs", NEW_TOP_SONGS))
            .collect();
        if items.is_empty() {
            continue;
        }
        let shelf = Shelf {
            title: format!("New from {}", artist.name),
            items,
            list: false,
            more: None,
        };
        ui.push_id(("new-from", &artist.id), |ui| {
            widgets::shelf(ui, app, &shelf, actions)
        });
    }
}

const NEW_SINGLES: usize = 6;
const NEW_ALBUMS: usize = 4;
const NEW_TOP_SONGS: usize = 6;

pub fn search(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    // An emptied field goes back to the start: the recent searches.
    if app.search_query.is_empty() || app.search_text.trim().is_empty() {
        title(ui, "Search", 34.0);
        ui.add_space(6.0);
        ui.label(
            RichText::new("Type in the field above and press Enter. ⌘F / Ctrl+F jumps there.")
                .color(theme::SECONDARY),
        );
        recent_searches(ui, app, actions);
        return;
    }
    title(ui, &format!("“{}”", app.search_query), 30.0);
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        for filter in SearchFilter::ALL {
            let selected = app.search_filter == filter;
            let text = RichText::new(filter.label())
                .font(theme::semibold(13.5))
                .color(if selected { theme::BG } else { theme::TEXT });
            let fill = if selected {
                theme::TEXT
            } else {
                theme::SURFACE_HOVER
            };
            if ui
                .add(
                    egui::Button::new(text)
                        .fill(fill)
                        .corner_radius(16.0)
                        .min_size(vec2(0.0, 32.0)),
                )
                .clicked()
            {
                actions.push(Action::SearchFilter(filter));
            }
        }
    });
    ui.add_space(20.0);
    let key = (app.search_query.clone(), app.search_filter);
    let results = match app.searches.get(&key) {
        Some(Loadable::Loaded(results)) => results,
        Some(Loadable::Failed(error)) => return failed(ui, error, Page::Search, actions),
        _ => return loading(ui),
    };
    let since = since(ui, &key);
    if results.top.is_none() && results.shelves.is_empty() {
        return widgets::empty(ui, "Nothing found.");
    }
    if app.search_filter != SearchFilter::All {
        let items: Vec<Item> = results
            .shelves
            .iter()
            .flat_map(|s| s.items.iter().cloned())
            .collect();
        ui.scope(|ui| {
            reveal(ui, since, 0);
            if items.iter().all(Item::is_playable) {
                if let Some(at) = track_list(ui, app, &items, false, actions) {
                    actions.push(Action::OpenItem(items[at].clone()));
                }
            } else {
                card_grid(ui, &items, app, actions);
            }
        });
        return;
    }
    if let Some(top) = &results.top {
        ui.scope(|ui| {
            reveal(ui, since, 0);
            top_result(ui, top, actions);
        });
        ui.add_space(28.0);
    }
    for (i, shelf) in results.shelves.iter().enumerate() {
        ui.scope(|ui| {
            reveal(ui, since, i + 1);
            widgets::shelf(ui, app, shelf, actions);
        });
    }
}

fn top_result(ui: &mut Ui, item: &Item, actions: &mut Vec<Action>) {
    egui::Frame::new()
        .fill(theme::SURFACE)
        .corner_radius(14.0)
        .inner_margin(18)
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(720.0));
            ui.horizontal(|ui| {
                let size = Vec2::splat(132.0);
                let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                let radius = if item.kind == Kind::Artist { 66.0 } else { 8.0 };
                widgets::paint_art(ui, rect, &item.thumbnails, radius);
                if response.clicked() {
                    actions.push(Action::OpenItem(item.clone()));
                }
                ui.add_space(12.0);
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("TOP RESULT")
                            .font(theme::semibold(11.0))
                            .color(theme::ACCENT_HOVER),
                    );
                    title(ui, &item.title, 28.0);
                    ui.add(
                        Label::new(RichText::new(&item.subtitle).color(theme::SECONDARY))
                            .truncate(),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| match item.kind {
                        Kind::Song | Kind::Video => {
                            if pill(ui, Some(Icon::Play), "Play", true).clicked() {
                                actions.push(Action::OpenItem(item.clone()));
                            }
                            if pill(ui, Some(Icon::Radio), "Radio", false).clicked() {
                                actions.push(Action::StartRadio(item.clone()));
                            }
                        }
                        Kind::Album | Kind::Playlist => {
                            if pill(ui, Some(Icon::Play), "Play", true).clicked() {
                                actions.push(Action::PlayCollection {
                                    id: item.id.clone(),
                                    shuffle: false,
                                });
                            }
                            if pill(ui, None, "Open", false).clicked() {
                                actions.push(Action::OpenItem(item.clone()));
                            }
                        }
                        Kind::Artist => {
                            if pill(ui, None, "Open", true).clicked() {
                                actions.push(Action::OpenItem(item.clone()));
                            }
                        }
                    });
                });
            });
        });
}

/// Track rows, drawing only the ones on screen so a playlist of thousands
/// costs what a screenful does. Returns the row clicked.
fn track_list(
    ui: &mut Ui,
    app: &App,
    tracks: &[Item],
    numbered: bool,
    actions: &mut Vec<Action>,
) -> Option<usize> {
    let mut clicked = None;
    let playing = app.current().map(|t| t.id.clone());
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let top = ui.cursor().top();
        let clip = ui.clip_rect();
        let row = theme::ROW_HEIGHT;
        let first = (((clip.top() - top) / row).floor().max(0.0) as usize).min(tracks.len());
        let last = ((((clip.bottom() - top) / row).ceil().max(0.0) as usize) + 1)
            .min(tracks.len())
            .max(first);
        ui.add_space(first as f32 * row);
        for (at, track) in tracks.iter().enumerate().take(last).skip(first) {
            let lead = if numbered {
                Lead::Number(at + 1)
            } else {
                Lead::Art
            };
            let is_playing = playing.as_deref() == Some(track.id.as_str());
            if widgets::track_row(ui, track, lead, is_playing, app, actions).play {
                clicked = Some(at);
            }
        }
        ui.add_space((tracks.len() - last) as f32 * row);
    });
    clicked
}

fn collection_header(
    ui: &mut Ui,
    id: &str,
    collection: &Collection,
    album: bool,
    actions: &mut Vec<Action>,
) {
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(210.0), Sense::hover());
        widgets::paint_art(ui, rect, &collection.thumbnails, 10.0);
        ui.add_space(18.0);
        ui.vertical(|ui| {
            ui.add_space(14.0);
            let kind = if album { "ALBUM" } else { "PLAYLIST" };
            ui.label(
                RichText::new(kind)
                    .font(theme::semibold(11.0))
                    .color(theme::ACCENT_HOVER),
            );
            title(ui, &collection.title, 38.0);
            ui.add_space(4.0);
            if collection.artists.is_empty() {
                ui.label(RichText::new(&collection.subtitle).color(theme::SECONDARY));
            } else {
                ui.horizontal(|ui| {
                    for artist in &collection.artists {
                        let text = RichText::new(&artist.name)
                            .font(theme::semibold(14.0))
                            .color(theme::TEXT);
                        if let Some(id) = &artist.id
                            && ui.add(egui::Link::new(text)).clicked()
                        {
                            actions.push(Action::Open(Page::Artist(id.clone())));
                        }
                    }
                    let year: String = collection
                        .subtitle
                        .rsplit('•')
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    if year.len() == 4 && year.chars().all(|c| c.is_ascii_digit()) {
                        ui.label(RichText::new(format!("• {year}")).color(theme::SECONDARY));
                    }
                });
            }
            ui.label(
                RichText::new(&collection.info)
                    .font(theme::body(13.0))
                    .color(theme::DIM),
            );
            if !collection.description.is_empty() {
                ui.add_space(4.0);
                let short: String = collection.description.chars().take(240).collect();
                ui.add(
                    Label::new(
                        RichText::new(short)
                            .font(theme::body(13.0))
                            .color(theme::SECONDARY),
                    )
                    .wrap(),
                );
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let tracks = &collection.tracks;
                if pill(ui, Some(Icon::Play), "Play", true).clicked() && !tracks.is_empty() {
                    actions.push(Action::PlayCollection {
                        id: id.to_string(),
                        shuffle: false,
                    });
                }
                if pill(ui, Some(Icon::Shuffle), "Shuffle", false).clicked() && !tracks.is_empty() {
                    actions.push(Action::PlayCollection {
                        id: id.to_string(),
                        shuffle: true,
                    });
                }
            });
        });
    });
}

pub fn collection(ui: &mut Ui, app: &App, id: &str, page: &Page, actions: &mut Vec<Action>) {
    let album = matches!(page, Page::Album(_));
    let collection = match app.collections.get(id) {
        Some(Loadable::Loaded(collection)) => collection,
        Some(Loadable::Failed(error)) => return failed(ui, error, page.clone(), actions),
        _ => return loading(ui),
    };
    ui.add_space(12.0);
    collection_header(ui, id, collection, album, actions);
    ui.add_space(24.0);
    if collection.tracks.is_empty() {
        return widgets::empty(ui, "No tracks here.");
    }
    if let Some(at) = track_list(ui, app, &collection.tracks, album, actions) {
        actions.push(Action::PlayAll {
            tracks: collection.tracks.clone(),
            start: at,
            source: Some(id.to_string()),
        });
    }
    if collection.continuation.is_some() {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| ui.add(egui::Spinner::new().color(theme::ACCENT)));
    }
}

pub fn artist(ui: &mut Ui, app: &App, id: &str, actions: &mut Vec<Action>) {
    let artist = match app.artists.get(id) {
        Some(Loadable::Loaded(artist)) => artist,
        Some(Loadable::Failed(error)) => {
            return failed(ui, error, Page::Artist(id.to_string()), actions);
        }
        _ => return loading(ui),
    };
    let width = ui.available_width();
    let height = (width * 0.34).clamp(220.0, 340.0);
    let (banner, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    widgets::paint_art(ui, banner, &artist.thumbnails, 14.0);
    let fade = Rect::from_min_max(
        egui::pos2(banner.min.x, banner.center().y - 20.0),
        banner.max,
    );
    theme::gradient(
        ui.painter(),
        fade,
        Color32::TRANSPARENT,
        Color32::from_black_alpha(230),
    );
    let name_pos = banner.left_bottom() + vec2(24.0, -64.0);
    ui.painter().text(
        name_pos,
        egui::Align2::LEFT_BOTTOM,
        &artist.name,
        theme::display(52.0),
        theme::TEXT,
    );
    ui.painter().text(
        name_pos + vec2(2.0, 30.0),
        egui::Align2::LEFT_BOTTOM,
        &artist.audience,
        theme::body(14.0),
        theme::SECONDARY,
    );
    ui.add_space(16.0);
    ui.horizontal(|ui| {
        if let Some(shuffle) = &artist.shuffle
            && pill(ui, Some(Icon::Shuffle), "Shuffle", true).clicked()
        {
            actions.push(Action::PlayWatch(shuffle.clone()));
        }
        if let Some(radio) = &artist.radio
            && pill(ui, Some(Icon::Radio), "Radio", false).clicked()
        {
            actions.push(Action::PlayWatch(radio.clone()));
        }
    });
    if !artist.description.is_empty() {
        ui.add_space(14.0);
        let short: String = artist.description.chars().take(320).collect();
        let more = if artist.description.chars().count() > 320 {
            "…"
        } else {
            ""
        };
        ui.add(Label::new(RichText::new(format!("{short}{more}")).color(theme::SECONDARY)).wrap());
    }
    ui.add_space(28.0);
    let since = since(ui, ("artist", id));
    for (i, shelf) in artist.shelves.iter().enumerate() {
        ui.scope(|ui| {
            reveal(ui, since, i);
            widgets::shelf(ui, app, shelf, actions);
        });
    }
}

pub fn feed(ui: &mut Ui, app: &App, target: &Browse, actions: &mut Vec<Action>) {
    let feed = match app.feeds.get(target) {
        Some(Loadable::Loaded(feed)) => feed,
        Some(Loadable::Failed(error)) => {
            return failed(ui, error, Page::Browse(target.clone()), actions);
        }
        _ => return loading(ui),
    };
    let heading = if feed.title.is_empty() {
        feed.shelves
            .first()
            .map(|s| s.title.as_str())
            .unwrap_or_default()
    } else {
        &feed.title
    };
    ui.add_space(8.0);
    title(ui, heading, 34.0);
    ui.add_space(18.0);
    if let [shelf] = feed.shelves.as_slice() {
        if shelf.list {
            if let Some(at) = track_list(ui, app, &shelf.items, false, actions) {
                actions.push(Action::PlayAll {
                    tracks: shelf.items.clone(),
                    start: at,
                    source: None,
                });
            }
        } else {
            card_grid(ui, &shelf.items, app, actions);
        }
    } else {
        for shelf in &feed.shelves {
            widgets::shelf(ui, app, shelf, actions);
        }
    }
    if feed.continuation.is_some() {
        load_more(ui, Action::LoadMoreFeed(target.clone()), actions);
    }
}

pub fn library(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    ui.add_space(8.0);
    title(ui, "Library", 34.0);
    ui.add_space(14.0);
    if !app.signed_in {
        ui.label(
            RichText::new("Sign in to see your playlists, albums and artists.")
                .color(theme::SECONDARY),
        );
        ui.add_space(12.0);
        if pill(ui, Some(Icon::User), "Sign in", true).clicked() {
            actions.push(Action::Open(Page::Settings));
        }
        return;
    }
    const TABS: [(&str, &str); 3] = [
        (LIBRARY_PLAYLISTS, "Playlists"),
        (LIBRARY_ALBUMS, "Albums"),
        (LIBRARY_ARTISTS, "Artists"),
    ];
    let key = Id::new("library-tab");
    let mut tab = ui.data(|d| d.get_temp::<usize>(key)).unwrap_or(0);
    ui.horizontal(|ui| {
        for (i, (_, label)) in TABS.iter().enumerate() {
            let selected = tab == i;
            let text = RichText::new(*label)
                .font(theme::semibold(13.5))
                .color(if selected { theme::BG } else { theme::TEXT });
            let fill = if selected {
                theme::TEXT
            } else {
                theme::SURFACE_HOVER
            };
            if ui
                .add(
                    egui::Button::new(text)
                        .fill(fill)
                        .corner_radius(16.0)
                        .min_size(vec2(0.0, 32.0)),
                )
                .clicked()
            {
                tab = i;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(key, tab));
    ui.add_space(20.0);
    let (browse_id, _) = TABS[tab];
    match app.library.get(browse_id) {
        Some(Loadable::Loaded(items)) if items.is_empty() => {
            widgets::empty(ui, "Nothing here yet.")
        }
        Some(Loadable::Loaded(items)) => {
            ui.scope(|ui| {
                reveal(ui, since(ui, ("library", browse_id)), 0);
                card_grid(ui, items, app, actions);
            });
        }
        Some(Loadable::Failed(error)) => failed(ui, error, Page::Library, actions),
        _ => loading(ui),
    }
}
