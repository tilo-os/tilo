//! Data usage: what this device moved over each connection, as the session
//! counts it into the `network_usage` section. The page shows the days of
//! a span as stacked bars, one colour per connection (the exact amounts
//! under the pointer), and every connection's total with a way to forget
//! it; the modem's page shows the mobile data period against its reset day
//! and the plan's allowance.

use chrono::NaiveDate;
use provider_settings_ui::bound::{SettingRows as _, record_item};
use provider_settings_ui::widgets::{El, Item, SettingsSection, block, card, note, row, rows};
use ui::LocalState;
use ui::framework::ViewCx;
use ui::icons::Icon;
use ui::widget::{
    Button, ButtonRole, Category, CategoryChart, CategoryChartStyle, CategoryLayout,
    CategorySeries, CategoryValue, Column, Container, ListRow, Row, RowMenu, Segment, Spacer, Text,
    ToggleGroup,
};

use super::Page;
use crate::i18n::{t, t_args};
use crate::prefs::{ConnectionKind, NetworkUsage};
use settings_sections::network_usage::{Bytes, ConnectionUsage};

/// The days the chart shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Span {
    Days7,
    Days30,
    /// The mobile data period, from its reset day.
    Period,
}

impl Span {
    pub(super) const ALL: [Self; 3] = [Self::Days7, Self::Days30, Self::Period];
    /// The span shown first.
    pub(crate) const FIRST: usize = 1;

    fn of(index: usize) -> Self {
        Self::ALL.get(index).copied().unwrap_or(Self::Days30)
    }

    fn label(self) -> String {
        t(match self {
            Self::Days7 => "net-usage-span-7",
            Self::Days30 => "net-usage-span-30",
            Self::Period => "net-usage-span-period",
        })
    }

    /// The first and last day shown on `today`.
    fn days(self, usage: &NetworkUsage, today: NaiveDate) -> (NaiveDate, NaiveDate) {
        let back = |days: u64| {
            today
                .checked_sub_days(chrono::Days::new(days))
                .unwrap_or(today)
        };
        let from = match self {
            Self::Days7 => back(6),
            Self::Days30 => back(29),
            Self::Period => usage.reset_day.period_start(today),
        };
        (from, today)
    }
}

/// Today, on this device's clock.
fn today() -> NaiveDate {
    ui::clock::local_now().date_naive()
}

/// How many connections the chart shows by themselves; the rest are one.
const SHOWN: usize = 5;

/// A connection's name as the person knows it: a wired profile's own name
/// only when there are several.
fn name_of(usage: &NetworkUsage, connection: &ConnectionUsage) -> String {
    let wired = usage
        .connections
        .values()
        .filter(|other| other.kind == ConnectionKind::Wired)
        .count();
    match connection.kind {
        ConnectionKind::Wired if wired < 2 => t("net-ethernet"),
        _ if connection.name.is_empty() => t("net-usage-unnamed"),
        _ => connection.name.clone(),
    }
}

fn icon_of(kind: ConnectionKind) -> Icon {
    match kind {
        ConnectionKind::Wifi => Icon::Wifi,
        ConnectionKind::Wired => Icon::EthernetPort,
        ConnectionKind::Mobile => Icon::Globe,
    }
}

/// The connections with traffic from `from` to `to`, the most first, each
/// with its key and what it moved then.
fn ranked(
    usage: &NetworkUsage,
    from: NaiveDate,
    to: NaiveDate,
    only: Option<ConnectionKind>,
) -> Vec<(&str, &ConnectionUsage, Bytes)> {
    let mut ranked: Vec<_> = usage
        .connections
        .iter()
        .filter(|(_, connection)| only.is_none_or(|kind| connection.kind == kind))
        .map(|(key, connection)| (key.as_str(), connection, connection.between(from, to)))
        .filter(|(_, _, bytes)| bytes.total() > 0)
        .collect();
    ranked.sort_by(|a, b| b.2.total().cmp(&a.2.total()).then(a.0.cmp(b.0)));
    ranked
}

/// What one bar says under the pointer.
fn detail(bytes: Bytes) -> String {
    t_args(
        "net-usage-detail",
        &[
            ("total", ui::format::size(bytes.total()).into()),
            ("received", ui::format::size(bytes.received).into()),
            ("sent", ui::format::size(bytes.sent).into()),
        ],
    )
}

/// A day's label under its bar: the weekday in a week, else the date.
fn day_label(day: NaiveDate, days: usize) -> String {
    if days <= 7 {
        ui::format::weekday_short(chrono::Datelike::weekday(&day))
    } else {
        ui::format::month_day(day)
    }
}

/// The days from `from` to `to` as stacked bars, one series per connection
/// (the [`SHOWN`] that moved the most, the rest as one).
pub(super) fn chart(
    usage: &NetworkUsage,
    from: NaiveDate,
    to: NaiveDate,
    only: Option<ConnectionKind>,
) -> CategoryChart {
    let days: Vec<NaiveDate> = from.iter_days().take_while(|day| *day <= to).collect();
    let categories = days
        .iter()
        .map(|day| {
            Category::new(
                settings_sections::network_usage::day_key(*day),
                day_label(*day, days.len()),
            )
        })
        .collect();
    let ranked = ranked(usage, from, to, only);
    let mut series: Vec<CategorySeries> = ranked
        .iter()
        .take(SHOWN)
        .enumerate()
        .map(|(index, (key, connection, _))| {
            let values = days
                .iter()
                .map(|day| {
                    let bytes = connection.on(*day);
                    CategoryValue::new(ui::num::f64_of_u64(bytes.total()))
                        .with_detail(detail(bytes))
                })
                .collect();
            CategorySeries::from_palette(*key, name_of(usage, connection), index, values)
        })
        .collect();
    let rest = ranked.get(SHOWN..).unwrap_or_default();
    if !rest.is_empty() {
        let values = days
            .iter()
            .map(|day| {
                let mut bytes = Bytes::default();
                for (_, connection, _) in rest {
                    let on = connection.on(*day);
                    bytes.received = bytes.received.saturating_add(on.received);
                    bytes.sent = bytes.sent.saturating_add(on.sent);
                }
                CategoryValue::new(ui::num::f64_of_u64(bytes.total())).with_detail(detail(bytes))
            })
            .collect();
        series.push(CategorySeries::from_palette(
            "other",
            t("net-usage-other"),
            SHOWN,
            values,
        ));
    }
    let single = series.len() < 2;
    CategoryChart::new(categories, series)
        .with_style(CategoryChartStyle {
            layout: CategoryLayout::Stacked,
            show_legend: !single,
            value_format: ui::charts::AxisLabelFormat::Size,
            corner_radius: 3.0,
            ..CategoryChartStyle::default()
        })
        .with_height(if single { 200.0 } else { 240.0 })
}

/// A large figure with what it measures under it.
fn figure(value: &str, caption: &str) -> El {
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight)
            .push(Box::new(
                Text::new(value)
                    .with_role(ui::TextRole::LargeTitle)
                    .numeric(),
            ))
            .push(Box::new(Text::new(caption).dimmed())),
    )
}

/// The overview: the span's total, the span switch and the chart.
fn overview(cx: &ViewCx<NetworkUsage>, span: &LocalState<usize>) -> Item {
    let usage = cx.value(|usage: &NetworkUsage| usage.clone());
    let span = span.clone();
    block(cx.switch(&usage, move |cx, usage| {
        let (usage, spans) = (usage.clone(), span.clone());
        cx.switch(&span, move |_, index| {
            let shown = Span::of(*index);
            let today = today();
            let (from, to) = shown.days(&usage, today);
            let total = ranked(&usage, from, to, None)
                .iter()
                .map(|(_, _, bytes)| bytes.total())
                .fold(0, u64::saturating_add);
            let switch = ToggleGroup::new(
                Span::ALL
                    .iter()
                    .map(|span| Segment::new(span.label()))
                    .collect(),
                *index,
            )
            .with_id("network.usage.span")
            .with_accessible_name(t("net-usage-span"))
            .bind_selection(&spans);
            let head = Row::new()
                .no_wrap()
                .with_align_items(taffy::AlignItems::End)
                .push(Box::new(
                    Container::new()
                        .with_no_padding()
                        .with_flex_grow()
                        .with_child(figure(
                            &ui::format::size(total),
                            &ui::format::day_range(from, to),
                        )),
                ))
                .push(Box::new(switch));
            let mut column = Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_rhythm(ui::tokens::Rhythm::Related)
                .with_id("network.usage.overview")
                .push(Box::new(head));
            if total == 0 {
                column = column.push(Box::new(Text::new(&t("net-usage-none")).dimmed()));
            } else {
                column = column.push(Box::new(chart(&usage, from, to, None)));
            }
            Box::new(column)
        })
    }))
}

/// Every connection with traffic in the span, the most first: its total,
/// what came in and went out, and forgetting it.
fn connections(cx: &ViewCx<NetworkUsage>, span: &LocalState<usize>) -> Item {
    let usage = cx.value(|usage: &NetworkUsage| usage.clone());
    let span = span.clone();
    block(cx.switch(&usage, move |cx, usage| {
        let usage = usage.clone();
        cx.switch(&span, move |cx, index| {
            let (from, to) = Span::of(*index).days(&usage, today());
            let ranked = ranked(&usage, from, to, None);
            if ranked.is_empty() {
                return Box::new(Text::new(&t("net-usage-none-yet")).dimmed());
            }
            let actions = cx.actions();
            let lines: Vec<El> = ranked
                .iter()
                .map(|(key, connection, bytes)| {
                    let name = name_of(&usage, connection);
                    let menu = RowMenu::new(
                        format!("settings.network.usage.menu.{key}"),
                        &t_args("net-more-for", &[("name", name.as_str().into())]),
                    )
                    .destructive(
                        t("net-usage-forget"),
                        actions.invocation(
                            NetworkUsage::FORGET_DATA_USED.with(Some((*key).to_owned())),
                        ),
                    );
                    Box::new(
                        ListRow::new(format!("settings.network.usage.{key}"), name)
                            .icon(icon_of(connection.kind))
                            .secondary(t_args(
                                "net-usage-in-out",
                                &[
                                    ("received", ui::format::size(bytes.received).into()),
                                    ("sent", ui::format::size(bytes.sent).into()),
                                ],
                            ))
                            .value(ui::format::size(bytes.total()))
                            .trailing(Box::new(menu)),
                    ) as El
                })
                .collect();
            Box::new(ui::widget::Section::new(lines))
        })
    }))
}

impl Page<'_> {
    /// The data usage page: the overview, then each connection.
    pub(super) fn usage_page(&self) -> Vec<SettingsSection> {
        let record = self.cx.settings::<NetworkUsage>();
        let span = &self.usage_span;
        vec![
            card(
                &t("net-usage"),
                rows(
                    record_item(self.cx, &record, |cx| overview(cx, span))
                        .into_iter()
                        .collect(),
                ),
            ),
            card(
                &t("net-usage-by-connection"),
                rows(
                    record_item(self.cx, &record, |cx| connections(cx, span))
                        .into_iter()
                        .chain([note(&t("net-usage-caption"))])
                        .collect(),
                ),
            ),
        ]
    }

    /// What the data usage row says on the first page: the last 30 days.
    pub(super) fn usage_summary(&self) -> Option<String> {
        let usage = self.reach.storage.settings::<NetworkUsage>().get();
        let (from, to) = Span::Days30.days(&usage, today());
        let total = ranked(&usage, from, to, None)
            .iter()
            .map(|(_, _, bytes)| bytes.total())
            .fold(0, u64::saturating_add);
        (total > 0).then(|| ui::format::size(total))
    }

    /// What one saved network or wired profile moved in the last 30 days,
    /// as a row of its page.
    pub(super) fn usage_row(&self, connection: &str) -> Option<Item> {
        let usage = self.reach.storage.settings::<NetworkUsage>().get();
        let counted = usage.connections.get(connection)?;
        let (from, to) = Span::Days30.days(&usage, today());
        let bytes = counted.between(from, to);
        Some(row(Box::new(
            ListRow::new("settings.network.page.usage", t("net-usage-30"))
                .secondary(t_args(
                    "net-usage-in-out",
                    &[
                        ("received", ui::format::size(bytes.received).into()),
                        ("sent", ui::format::size(bytes.sent).into()),
                    ],
                ))
                .value(ui::format::size(bytes.total())),
        )))
    }
}

/// The mobile data period on the modem's page: what was used since the
/// period started against the plan's allowance, the days as bars, starting
/// the count again, the day the period starts on and the allowance.
pub(super) fn mobile_rows(cx: &ViewCx<crate::prefs::DeviceVpnPrefs>) -> Vec<Item> {
    let record = cx.settings::<NetworkUsage>();
    [
        record_item(cx, &record, mobile_period),
        record_item(cx, &record, |cx| {
            cx.setting(t("net-mobile-reset-day"))
                .id("network.mobile.reset-day")
                .among(
                    NetworkUsage::paths().reset_day,
                    crate::prefs::ResetDay::all(),
                    |day: &crate::prefs::ResetDay| day.get().to_string(),
                )
        }),
        record_item(cx, &record, |cx| {
            cx.setting(t("net-mobile-limit"))
                .id("network.mobile.limit")
                .among(
                    NetworkUsage::paths().limit_gb,
                    NetworkUsage::limits(),
                    |gb: &u32| {
                        if *gb == 0 {
                            t("net-mobile-limit-none")
                        } else {
                            ui::format::size(u64::from(*gb).saturating_mul(1_000_000_000))
                        }
                    },
                )
        }),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn mobile_period(cx: &ViewCx<NetworkUsage>) -> Item {
    let usage = cx.value(|usage: &NetworkUsage| usage.clone());
    block(cx.switch(&usage, |cx, usage| {
        let today = today();
        let used = usage.mobile_used(today);
        let from = usage.reset_day.period_start(today);
        let next = usage.reset_day.next_period(today);
        let reset = cx.actions().bind_invocation(
            Button::new(
                "settings.network.mobile.used.reset",
                &t("net-mobile-used-reset"),
            )
            .role(ButtonRole::Secondary),
            NetworkUsage::RESET_DATA_USED.with(None),
        );
        let head = Row::new()
            .no_wrap()
            .with_align_items(taffy::AlignItems::End)
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_flex_grow()
                    .with_child(figure(
                        &ui::format::size(used),
                        &t_args(
                            "net-mobile-used-since",
                            &[("date", ui::format::month_day(from).into())],
                        ),
                    )),
            ))
            .push(Box::new(reset));
        let mut column = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related)
            .with_id("settings.network.mobile.used")
            .push(Box::new(head));
        if let Some(limit) = usage.limit() {
            let share = ui::num::f64_of_u64(used) / ui::num::f64_of_u64(limit).max(1.0);
            let left = limit.saturating_sub(used);
            column = column.push(Box::new(
                ui::widget::BarGauge::new(ui::num::f32_of_f64(share))
                    .with_label(
                        t_args(
                            "net-mobile-limit-of",
                            &[("limit", ui::format::size(limit).into())],
                        ),
                        if used > limit {
                            t_args(
                                "net-mobile-over",
                                &[("over", ui::format::size(used - limit).into())],
                            )
                        } else {
                            t_args(
                                "net-mobile-left",
                                &[("left", ui::format::size(left).into())],
                            )
                        },
                    )
                    .with_thresholds(ui::charts::GaugeThresholds {
                        warning: 0.8,
                        danger: 1.0,
                    }),
            ));
        }
        if ranked(usage, from, today, Some(ConnectionKind::Mobile)).is_empty() {
            column = column.push(Box::new(Text::new(&t("net-usage-none")).dimmed()));
        } else {
            column = column.push(Box::new(chart(
                usage,
                from,
                today,
                Some(ConnectionKind::Mobile),
            )));
        }
        column = column.push(Box::new(
            Row::new()
                .push(Box::new(
                    Text::new(&t_args(
                        "net-mobile-next-period",
                        &[("date", ui::format::month_day(next).into())],
                    ))
                    .dimmed(),
                ))
                .push(Box::new(Spacer::new())),
        ));
        Box::new(column)
    }))
}

/// What the network captures show counted: a home network used daily, a
/// library on weekdays, a hotel for a few days, the office cable on
/// weekdays and a SIM with a plan, over the last six weeks. Only ever
/// written into a capture's own profile.
pub(crate) fn sample(storage: &ui::storage::AppStorage) {
    use settings_sections::network_usage::{ResetDay, day_key};
    let today = today();
    let mut usage = NetworkUsage {
        reset_day: ResetDay::new(15).unwrap_or_default(),
        limit_gb: 20,
        ..NetworkUsage::default()
    };
    let mut add = |key: &str, kind, name: &str, day: NaiveDate, megabytes: u64| {
        if megabytes == 0 {
            return;
        }
        let connection = usage.connections.entry(key.to_owned()).or_default();
        connection.kind = kind;
        name.clone_into(&mut connection.name);
        connection.days.insert(
            day_key(day),
            Bytes {
                received: megabytes.saturating_mul(900_000),
                sent: megabytes.saturating_mul(100_000),
            },
        );
    };
    for back in 0..42_u64 {
        let Some(day) = today.checked_sub_days(chrono::Days::new(back)) else {
            continue;
        };
        let weekday = chrono::Datelike::weekday(&day).num_days_from_monday();
        let wave = (back * 37 + 11) % 17;
        let workday = weekday < 5;
        add(
            "wifi:5b2e8a70-1c7d-4f2e-9a61-0d3c9e4b7a10",
            ConnectionKind::Wifi,
            "Home_Network",
            day,
            if workday {
                700 + wave * 90
            } else {
                2_100 + wave * 160
            },
        );
        add(
            "wired:0f1e2d3c-4b5a-4968-8776-655443322110",
            ConnectionKind::Wired,
            "Wired connection 1",
            day,
            if workday { 900 + wave * 60 } else { 0 },
        );
        add(
            "wifi:9e41c2d8-6a35-4b9f-8c07-2f5d1e8a3b64",
            ConnectionKind::Wifi,
            "Library",
            day,
            if weekday == 2 || weekday == 5 {
                300 + wave * 25
            } else {
                0
            },
        );
        add(
            "wifi:3c8f9b21-7d44-4e0a-b5c6-1a2b3c4d5e6f",
            ConnectionKind::Wifi,
            "Hotel_Lisbon",
            day,
            if (9..13).contains(&back) {
                1_400 + wave * 70
            } else {
                0
            },
        );
        add(
            "mobile:8935101234567890123",
            ConnectionKind::Mobile,
            "Telia",
            day,
            120 + wave * 30 + if (9..13).contains(&back) { 900 } else { 0 },
        );
    }
    if let Err(error) = storage.settings::<NetworkUsage>().set(&usage) {
        tracing::warn!(target: "tilo::settings", %error, "the sample data usage was not written");
    }
}
