//! Colours, fonts, icons and the shapes every view shares.

use std::sync::Arc;

use egui::{Color32, CornerRadius, FontData, FontFamily, FontId, Stroke, epaint};
use fastframe_fonts::{FontSetup, Weight};

pub const BG: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0d);
pub const SIDEBAR: Color32 = Color32::from_rgb(0x10, 0x10, 0x13);
pub const SURFACE: Color32 = Color32::from_rgb(0x1a, 0x1a, 0x1f);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x25, 0x25, 0x2c);
pub const OUTLINE: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x33);
pub const RAISED: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x44);
pub const TEXT: Color32 = Color32::from_rgb(0xf3, 0xf3, 0xf5);
pub const SECONDARY: Color32 = Color32::from_rgb(0xa3, 0xa3, 0xad);
pub const DIM: Color32 = Color32::from_rgb(0x6c, 0x6c, 0x76);
/// YouTube's red.
pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0x00, 0x33);
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(0xff, 0x33, 0x5c);
/// The deep red the page glows with at its top.
pub const GLOW: Color32 = Color32::from_rgb(0x3d, 0x05, 0x14);

pub const SIDEBAR_WIDTH: f32 = 232.0;
pub const PLAYER_HEIGHT: f32 = 92.0;
/// Room the macOS traffic lights take over the sidebar's top.
pub const TRAFFIC_LIGHTS: f32 = if cfg!(target_os = "macos") { 22.0 } else { 0.0 };
/// The strip along the window's top that drags it, where there is no title bar.
pub const DRAG_STRIP: f32 = 56.0;
pub const SIDE_PANEL_WIDTH: f32 = 360.0;
pub const PAGE_MARGIN: f32 = 32.0;
pub const CARD_WIDTH: f32 = 176.0;
pub const ROW_HEIGHT: f32 = 56.0;
/// The display size of a page's title.
pub const PAGE_TITLE: f32 = 34.0;
pub const RADIUS: u8 = 8;

const DISPLAY: &str = "display";

fastframe_icons::icons! {
    /// Every icon the interface draws.
    pub enum Icon {
        prefix: "ytfast-icon-",
        directory: "../assets/icons/",
        Play => "play-filled",
        Pause => "pause-filled",
        Previous => "skip-back-filled",
        Next => "skip-forward-filled",
        Shuffle => "shuffle",
        Repeat => "repeat",
        RepeatOne => "repeat-1",
        Home => "house",
        Library => "library",
        Queue => "list-music",
        PlayNext => "list-start",
        AddToQueue => "list-end",
        AddTo => "list-plus",
        Radio => "radio",
        Lyrics => "mic-vocal",
        Like => "thumbs-up",
        Liked => "thumbs-up-filled",
        History => "history",
        Sliders => "sliders-horizontal",
        Search => lucide "search",
        Settings => lucide "settings",
        Volume => lucide "volume-2",
        Muted => lucide "volume-x",
        Back => lucide "chevron-left",
        Forward => lucide "chevron-right",
        Close => lucide "x",
        User => lucide "user",
        LogOut => lucide "log-out",
        Refresh => lucide "refresh-cw",
        Pin => lucide "pin",
        Unpin => lucide "pin-off",
        Trash => lucide "trash-2",
        Copy => lucide "copy",
        Plus => lucide "plus",
        Check => lucide "check",
        Up => lucide "chevron-up",
        Down => lucide "chevron-down",
    }
}

pub fn install(ctx: &egui::Context) {
    let mut fonts = FontSetup::default()
        .weights(&[Weight::Medium, Weight::SemiBold, Weight::Bold])
        .definitions();
    fonts.font_data.insert(
        DISPLAY.into(),
        Arc::new(FontData::from_static(include_bytes!(
            "../assets/fonts/ArchivoBlack-Regular.ttf"
        ))),
    );
    // Titles in Archivo Black; whatever script it lacks falls back to the
    // interface faces, CJK and Arabic included.
    let mut display = vec![DISPLAY.to_string()];
    display.extend(
        fonts
            .families
            .get(&FontFamily::Proportional)
            .cloned()
            .unwrap_or_default(),
    );
    fonts
        .families
        .insert(FontFamily::Name(DISPLAY.into()), display);
    ctx.set_fonts(fonts);

    egui_extras::install_image_loaders(ctx);
    fastframe_icons::install::<Icon>(ctx);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG;
    visuals.window_fill = SURFACE;
    visuals.window_stroke = Stroke::new(1.0, OUTLINE);
    visuals.extreme_bg_color = SURFACE;
    visuals.faint_bg_color = SIDEBAR;
    visuals.hyperlink_color = TEXT;
    visuals.selection.bg_fill = ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, TEXT);
    visuals.slider_trailing_fill = true;
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.menu_corner_radius = CornerRadius::same(10);
    let widgets = &mut visuals.widgets;
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, OUTLINE);
    for (state, fill) in [
        // One step above the cards they sit on, so boxes and rails show.
        (&mut widgets.inactive, SURFACE_HOVER),
        (&mut widgets.hovered, OUTLINE),
        (&mut widgets.active, RAISED),
        (&mut widgets.open, OUTLINE),
    ] {
        state.bg_fill = fill;
        state.weak_bg_fill = fill;
        state.bg_stroke = Stroke::NONE;
        state.fg_stroke = Stroke::new(1.5, TEXT);
        state.corner_radius = CornerRadius::same(RADIUS);
        state.expansion = 0.0;
    }
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.menu_margin = egui::Margin::same(6);
        // Text is plain: rows and cards under it keep the pointer and the
        // clicks.
        style.interaction.selectable_labels = false;
        style.text_styles.insert(egui::TextStyle::Body, body(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, medium(14.0));
        style.text_styles.insert(egui::TextStyle::Small, body(12.0));
    });
}

pub fn display(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(DISPLAY.into()))
}

pub fn body(size: f32) -> FontId {
    Weight::Regular.font_id(size)
}

pub fn medium(size: f32) -> FontId {
    Weight::Medium.font_id(size)
}

pub fn semibold(size: f32) -> FontId {
    Weight::SemiBold.font_id(size)
}

/// A vertical gradient from `top` to `bottom` across `rect`.
pub fn gradient(painter: &egui::Painter, rect: egui::Rect, top: Color32, bottom: Color32) {
    let mut mesh = epaint::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), top);
    mesh.colored_vertex(rect.left_bottom(), bottom);
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 3, 2);
    painter.add(mesh);
}
