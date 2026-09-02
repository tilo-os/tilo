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

fn profile_arguments(
    name: String,
    updates: bool,
    refresh: f64,
    volume: f64,
    region: String,
) -> serde_json::Map<String, serde_json::Value> {
    serde_json::Map::from_iter([
        ("name".into(), serde_json::json!(name)),
        ("updates".into(), serde_json::json!(updates)),
        ("refresh".into(), serde_json::json!(refresh)),
        ("volume".into(), serde_json::json!(volume)),
        ("region".into(), serde_json::json!(region)),
    ])
}

/// A complete local form → typed state → retained output loop.
#[expect(
    clippy::expect_used,
    reason = "the binding uses the sole action constructed immediately above"
)]
pub fn app_root() -> Box<dyn Widget> {
    crate::install_catalog();
    let name = LocalState::new(String::from("world"));
    let updates = LocalState::new(true);
    let refresh_minutes = LocalState::new(15.0);
    let volume = LocalState::new(0.65);
    let region = LocalState::new(String::from("Europe/Berlin"));
    let resource = profile_resource();
    let action = resource
        .actions
        .iter()
        .find(|action| action.id == "update-profile")
        .expect("generated profile resource exports update-profile");
    let name_binding = {
        let updates = updates.clone();
        let refresh = refresh_minutes.clone();
        let volume = volume.clone();
        let region = region.clone();
        qos_actions::ActionBinding::new(&resource, action, move |value| {
            profile_arguments(
                value,
                updates.get(),
                refresh.get(),
                volume.get(),
                region.get(),
            )
        })
        .expect("profile name produces complete action arguments")
    };
    let updates_binding = {
        let name = name.clone();
        let refresh = refresh_minutes.clone();
        let volume = volume.clone();
        let region = region.clone();
        qos_actions::ActionBinding::new(&resource, action, move |value| {
            profile_arguments(name.get(), value, refresh.get(), volume.get(), region.get())
        })
        .expect("profile updates choice produces complete action arguments")
    };
    let refresh_binding = {
        let name = name.clone();
        let updates = updates.clone();
        let volume = volume.clone();
        let region = region.clone();
        qos_actions::ActionBinding::new(&resource, action, move |value| {
            profile_arguments(name.get(), updates.get(), value, volume.get(), region.get())
        })
        .expect("profile refresh produces complete action arguments")
    };
    let volume_binding = {
        let name = name.clone();
        let updates = updates.clone();
        let refresh = refresh_minutes.clone();
        let region = region.clone();
        qos_actions::ActionBinding::new(&resource, action, move |value| {
            profile_arguments(
                name.get(),
                updates.get(),
                refresh.get(),
                value,
                region.get(),
            )
        })
        .expect("profile volume produces complete action arguments")
    };
    let region_binding = {
        let name = name.clone();
        let updates = updates.clone();
        let refresh = refresh_minutes.clone();
        let volume = volume.clone();
        qos_actions::ActionBinding::new(&resource, action, move |value| {
            profile_arguments(
                name.get(),
                updates.get(),
                refresh.get(),
                volume.get(),
                value,
            )
        })
        .expect("profile region produces complete action arguments")
    };
    let greeting = Text::from_state(&name, |name| {
        ui::i18n::I18nManager::format("starter-greeting", &[("name", name.into())])
    })
    .with_id("greeting");
    let input = TextInput::from_state(&name)
        .with_id("name-input")
        .with_placeholder("starter-name-placeholder")
        .on_action(name_binding);
    let updates_widget = Checkbox::from_state(&updates)
        .with_id("updates-checkbox")
        .on_action(updates_binding);
    let refresh = NumberInput::from_state(&refresh_minutes)
        .with_range(1.0, 120.0)
        .with_step(5.0)
        .with_suffix("min")
        .with_id("refresh-input")
        .on_action(refresh_binding);
    let volume_widget = Slider::from_state(&volume)
        .with_id("volume-slider")
        .on_action(volume_binding);
    let region_widget = Dropdown::from_state(
        vec![
            String::from("Europe/Berlin"),
            String::from("America/New_York"),
            String::from("Asia/Tokyo"),
        ],
        &region,
    )
    .with_id("region-select")
    .on_action(region_binding);

    let reset_binding = qos_actions::ActionBinding::static_json_for(
        &resource,
        "update-profile",
        serde_json::Map::from_iter([
            ("name".into(), serde_json::json!("world")),
            ("updates".into(), serde_json::json!(true)),
            ("refresh".into(), serde_json::json!(15.0)),
            ("volume".into(), serde_json::json!(0.65)),
            ("region".into(), serde_json::json!("Europe/Berlin")),
        ]),
    )
    .expect("generated defaults match the exported profile action");
    let reset = Button::new("reset-form", "Reset form", None).on_action(reset_binding);

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
    ui::widget::resource_actions::immediate(root, resource, move |payload| {
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
