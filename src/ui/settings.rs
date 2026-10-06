//! Settings: the account, playback, and the equaliser.

use egui::{Align, Frame, Layout, RichText, Ui, vec2};

use crate::app::{Action, App};
use crate::audio::{EQ_BANDS, EQ_RANGE, EqPreset};
use crate::auth::BROWSERS;
use crate::theme::{self, Icon};
use crate::ui::widgets::pill;

fn section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    ui.add_space(22.0);
    ui.label(
        RichText::new(title)
            .font(theme::display(20.0))
            .color(theme::TEXT),
    );
    ui.add_space(10.0);
    Frame::new()
        .fill(theme::SURFACE)
        .corner_radius(12.0)
        .inner_margin(18)
        .show(ui, |ui| {
            ui.set_width(ui.available_width().min(760.0));
            body(ui);
        });
}

fn note(ui: &mut Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .font(theme::body(12.5))
            .color(theme::DIM),
    );
}

pub fn show(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    ui.add_space(8.0);
    ui.label(
        RichText::new("Settings")
            .font(theme::display(34.0))
            .color(theme::TEXT),
    );

    section(ui, "Account", |ui| account(ui, app, actions));
    section(ui, "Playback", |ui| {
        let mut autoplay = app.settings.autoplay;
        if ui
            .checkbox(&mut autoplay, "Autoplay similar songs when the queue ends")
            .changed()
        {
            actions.push(Action::SetAutoplay(autoplay));
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            ui.label("Crossfade");
            let mut seconds = app.settings.crossfade;
            let slider = egui::Slider::new(&mut seconds, 0.0..=12.0)
                .step_by(1.0)
                .custom_formatter(|v, _| {
                    if v == 0.0 {
                        "Off".into()
                    } else {
                        format!("{v:.0} s")
                    }
                });
            if ui.add_sized(vec2(260.0, 24.0), slider).changed() {
                actions.push(Action::SetCrossfade(seconds));
            }
        });
        note(
            ui,
            "Overlaps the end of a track with the start of the next one.",
        );
    });
    section(ui, "Equalizer", |ui| equalizer(ui, app, actions));
    section(ui, "About", |ui| {
        ui.label(format!("ytfast {}", env!("CARGO_PKG_VERSION")));
        note(
            ui,
            "Audio comes through yt-dlp, which the app downloads and keeps up to date in its data folder. \
                  An unofficial client, not affiliated with Google or YouTube.",
        );
    });
}

fn account(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    if app.signed_in {
        ui.horizontal(|ui| {
            ui.add(Icon::User.image(theme::SECONDARY, 20.0));
            match &app.account {
                Some(account) => {
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&account.name).font(theme::semibold(15.0)));
                        ui.label(RichText::new(&account.email).color(theme::SECONDARY));
                    });
                }
                None => _ = ui.label("Signed in"),
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if pill(ui, Some(Icon::LogOut), "Sign out", false).clicked() {
                    actions.push(Action::SignOut);
                }
                if pill(ui, Some(Icon::Refresh), "Read again", false)
                    .on_hover_text("Read the session from the browser again")
                    .clicked()
                {
                    actions.push(Action::SignIn);
                }
            });
        });
        return;
    }
    ui.label("Sign in with the YouTube session of a browser on this computer.");
    note(
        ui,
        "Open music.youtube.com in that browser and sign in first. Chrome on macOS asks for your Keychain \
              password once; Safari needs Full Disk Access for ytfast.",
    );
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("browser")
            .selected_text(capitalize(&app.settings.browser))
            .show_ui(ui, |ui| {
                for browser in BROWSERS {
                    if ui
                        .selectable_label(app.settings.browser == browser, capitalize(browser))
                        .clicked()
                    {
                        actions.push(Action::SetBrowser(browser.to_string()));
                    }
                }
            });
        if app.signing_in {
            ui.add(egui::Spinner::new().color(theme::ACCENT));
            ui.label("Reading the session…");
        } else if pill(ui, Some(Icon::User), "Sign in", true).clicked() {
            actions.push(Action::SignIn);
        }
    });
    if let Some(error) = &app.sign_in_error {
        ui.add_space(8.0);
        ui.label(RichText::new(error).color(theme::ACCENT_HOVER));
    }
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn equalizer(ui: &mut Ui, app: &App, actions: &mut Vec<Action>) {
    let settings = &app.settings;
    ui.horizontal(|ui| {
        let mut enabled = settings.eq_enabled;
        if ui.checkbox(&mut enabled, "On").changed() {
            actions.push(Action::SetEqEnabled(enabled));
        }
        ui.add_space(12.0);
        egui::ComboBox::from_id_salt("eq-preset")
            .selected_text(settings.eq_preset.label())
            .show_ui(ui, |ui| {
                for preset in EqPreset::ALL {
                    if ui
                        .selectable_label(settings.eq_preset == preset, preset.label())
                        .clicked()
                    {
                        actions.push(Action::SetEqPreset(preset));
                    }
                }
            });
    });
    ui.add_space(14.0);
    let gains = settings.eq_gains();
    ui.add_enabled_ui(settings.eq_enabled, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 18.0;
            ui.spacing_mut().slider_width = 150.0;
            for (band, hz) in EQ_BANDS.iter().enumerate() {
                ui.vertical(|ui| {
                    let mut gain = gains[band];
                    let slider = egui::Slider::new(&mut gain, -EQ_RANGE..=EQ_RANGE)
                        .vertical()
                        .step_by(0.5)
                        .show_value(false);
                    if ui
                        .add(slider)
                        .on_hover_text(format!("{gain:+.1} dB"))
                        .changed()
                    {
                        actions.push(Action::SetEqBand(band, gain));
                    }
                    let label = if *hz >= 1000.0 {
                        format!("{}k", hz / 1000.0)
                    } else {
                        format!("{hz}")
                    };
                    ui.label(
                        RichText::new(label)
                            .font(theme::body(11.5))
                            .color(theme::SECONDARY),
                    );
                });
            }
        });
    });
}
