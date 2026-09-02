use ui::LocalState;
use ui::qos_actions;
use ui::tokens::TextRole;
use ui::widget::{
    Button, Checkbox, Dropdown, NumberInput, Slider, Text, TextInput, Widget, labeled_check,
    labeled_field,
};

fn update_profile(
    name: &LocalState<String>,
    updates: &LocalState<bool>,
    refresh: &LocalState<f64>,
    volume: &LocalState<f64>,
    region: &LocalState<String>,
    values: (String, bool, f64, f64, String),
) {
    name.set(values.0);
    updates.set(values.1);
    refresh.set(values.2);
    volume.set(values.3);
    region.set(values.4);
}

fn profile_resource() -> qos_actions::Resource {
    ui::widget::resource_actions::typed_resource(
        "generated.form",
        "Profile form",
        "update-profile",
        "Update profile",
        vec![
            qos_actions::ActionParam::required_owned(
                "name",
                qos_actions::ParamType::Text,
                "Display name",
            ),
            qos_actions::ActionParam::required_owned(
                "updates",
                qos_actions::ParamType::Boolean,
                "Receive updates",
            ),
            qos_actions::ActionParam::required_owned(
                "refresh",
                qos_actions::ParamType::Number,
                "Refresh minutes",
            ),
            qos_actions::ActionParam::required_owned(
                "volume",
                qos_actions::ParamType::Number,
                "Volume from 0 to 1",
            ),
            qos_actions::ActionParam::required_owned(
                "region",
                qos_actions::ParamType::Text,
                "Region",
            ),
        ],
    )
}

/// A complete local form → typed state → retained output loop.
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let name = LocalState::new(String::from("world"));
    let updates = LocalState::new(true);
    let refresh_minutes = LocalState::new(15.0);
    let volume = LocalState::new(0.65);
    let region = LocalState::new(String::from("Europe/Berlin"));
    let greeting = Text::from_state(&name, |name| {
        ui::i18n::I18nManager::format("starter-greeting", &[("name", name.into())])
    })
    .with_id("greeting");
    let input = TextInput::from_state(&name)
        .with_id("name-input")
        .with_placeholder("starter-name-placeholder");
    let updates_widget = Checkbox::from_state(&updates).with_id("updates-checkbox");
    let refresh = NumberInput::from_state(&refresh_minutes)
        .with_range(1.0, 120.0)
        .with_step(5.0)
        .with_suffix("min")
        .with_id("refresh-input");
    let volume_widget = Slider::from_state(&volume).with_id("volume-slider");
    let region_widget = Dropdown::from_state(
        vec![
            String::from("Europe/Berlin"),
            String::from("America/New_York"),
            String::from("Asia/Tokyo"),
        ],
        &region,
    )
    .with_id("region-select");

    let reset_name = name.clone();
    let reset_updates = updates.clone();
    let reset_refresh = refresh_minutes.clone();
    let reset_volume = volume.clone();
    let reset_region = region.clone();
    let reset = Button::new("reset-form", "Reset form", None).on_click(move || {
        update_profile(
            &reset_name,
            &reset_updates,
            &reset_refresh,
            &reset_volume,
            &reset_region,
            ("world".into(), true, 15.0, 0.65, "Europe/Berlin".into()),
        );
    });

    let root = Box::new(ui::column![
        Text::new("starter-title").with_role(TextRole::Title),
        labeled_field("starter-name-label", input),
        labeled_check("starter-updates-label", updates_widget),
        labeled_field("starter-refresh-label", refresh),
        labeled_field("starter-volume-label", volume_widget),
        labeled_field("starter-region-label", region_widget),
        greeting,
        reset,
    ]);
    ui::widget::resource_actions::immediate(root, profile_resource(), move |payload| {
        let args: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let values = (
            args.get("name")
                .and_then(|value| value.as_str())
                .ok_or("missing name")?
                .to_owned(),
            args.get("updates")
                .and_then(serde_json::Value::as_bool)
                .ok_or("missing updates")?,
            args.get("refresh")
                .and_then(serde_json::Value::as_f64)
                .ok_or("missing refresh")?,
            args.get("volume")
                .and_then(serde_json::Value::as_f64)
                .ok_or("missing volume")?,
            args.get("region")
                .and_then(|value| value.as_str())
                .ok_or("missing region")?
                .to_owned(),
        );
        update_profile(&name, &updates, &refresh_minutes, &volume, &region, values);
        Ok(qos_actions::ActionOutput::default())
    })
}
