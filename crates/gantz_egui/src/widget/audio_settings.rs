//! The "Audio" settings subtab: the audio runtime's status plus a couple of live
//! knobs (scheduling lead and mute). Supplied by the bevy app; absent in the
//! non-bevy demo, so the subtab does not appear there.

/// Audio status (read-only) + the editable live settings for the Audio subtab.
#[derive(Clone, Debug, Default)]
pub struct AudioPanel {
    /// Whether an audio output device is present (else the app runs silent).
    pub present: bool,
    /// The active output device's name.
    pub device: Option<String>,
    /// The output sample rate (Hz).
    pub sample_rate: f64,
    /// The number of output channels.
    pub channels: usize,
    /// The scheduling lead in milliseconds (latency vs sample-accurate timing).
    pub sched_lead_ms: f32,
    /// Whether audio output is enabled (unmuted).
    pub enabled: bool,
}

/// What the user changed in the Audio subtab this frame.
#[derive(Default)]
pub struct AudioSettingsResponse {
    /// The scheduling lead (ms) was changed to this value.
    pub sched_lead_ms: Option<f32>,
    /// The enable/mute toggle was changed to this value.
    pub enabled: Option<bool>,
}

/// Render the Audio settings subtab: a read-only status readout plus a live
/// scheduling-lead control and an enable/mute toggle.
pub fn audio_settings(panel: &AudioPanel, ui: &mut egui::Ui) -> AudioSettingsResponse {
    let mut res = AudioSettingsResponse::default();

    ui.label("Status:");
    if panel.present {
        egui::Grid::new("audio_status_grid")
            .num_columns(2)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                ui.label("Device");
                ui.label(panel.device.as_deref().unwrap_or("-"));
                ui.end_row();
                ui.label("Sample rate");
                ui.label(format!("{:.0} Hz", panel.sample_rate));
                ui.end_row();
                ui.label("Channels");
                ui.label(panel.channels.to_string());
                ui.end_row();
            });
    } else {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "No audio output device — running silent.",
        );
    }

    ui.separator();

    // The live controls only do anything when a device is present.
    ui.add_enabled_ui(panel.present, |ui| {
        let mut enabled = panel.enabled;
        if ui
            .checkbox(&mut enabled, "Audio enabled")
            .on_hover_text("Mute/unmute audio output (pauses the output stream).")
            .changed()
        {
            res.enabled = Some(enabled);
        }

        let mut lead = panel.sched_lead_ms;
        ui.horizontal(|ui| {
            let dv = egui::DragValue::new(&mut lead)
                .range(0.0..=500.0)
                .speed(1.0)
                .suffix(" ms");
            if ui
                .add(dv)
                .on_hover_text(
                    "Scheduling lead: how far ahead control automation is scheduled. \
                     Higher = safer timing but more latency; must exceed output latency \
                     or timing degrades to block granularity.",
                )
                .changed()
            {
                res.sched_lead_ms = Some(lead);
            }
            ui.label("Scheduling lead");
        });
    });

    res
}
