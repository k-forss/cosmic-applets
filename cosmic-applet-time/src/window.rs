// Copyright 2023 System76 <info@system76.com>
// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    Apply, Element, Task, app,
    applet::{cosmic_panel_config::PanelAnchor, menu_button, padded_control},
    cctk::sctk::reexports::calloop,
    cosmic_theme::Spacing,
    iced::{
        Alignment, Length, Rectangle, Subscription,
        futures::{SinkExt, StreamExt, channel::mpsc},
        platform_specific::shell::wayland::commands::popup::{destroy_popup, get_popup},
        widget::{column, row, rule},
        window,
    },
    iced_futures::stream,
    iced_widget::Column,
    surface, theme,
    widget::{
        Button, Grid, Id, autosize, button, container, divider, grid, icon, rectangle_tracker::*,
        space, text,
    },
};
use jiff::{
    Timestamp, ToSpan, Zoned,
    civil::{Date, Weekday},
    fmt::strtime,
    tz::TimeZone,
};
use logind_zbus::manager::ManagerProxy;
use std::hash::Hash;
use std::sync::LazyLock;
use timedate_zbus::TimeDateProxy;
use tokio::{sync::watch, time};

use crate::calendar::{
    self, CalendarConfig, CalendarEvent, CalendarTodo, CALENDAR_CONFIG_ID,
    config::SourceType,
};
use crate::{config::TimeAppletConfig, fl, time::get_calendar_first};
use cosmic::applet::token::subscription::{
    TokenRequest, TokenUpdate, activation_token_subscription,
};
use std::collections::BTreeMap;
use icu::{
    datetime::{
        DateTimeFormatter, DateTimeFormatterPreferences, fieldsets,
        input::{Date as IcuDate, DateTime, Time},
        options::TimePrecision,
    },
    locale::{Locale, preferences::extensions::unicode::keywords::HourCycle},
};

static AUTOSIZE_MAIN_ID: LazyLock<Id> = LazyLock::new(|| Id::new("autosize-main"));

// Specifiers for strftime that indicate seconds. Subsecond precision isn't supported by the applet
// so those specifiers aren't listed here. This list is non-exhaustive, and it's possible that %X
// and other specifiers have to be added depending on locales.
const STRFTIME_SECONDS: &[char] = &['S', 'T', '+', 's'];

fn get_system_locale() -> Locale {
    for var in ["LC_TIME", "LC_ALL", "LANG"] {
        if let Ok(locale_str) = std::env::var(var) {
            let cleaned_locale = locale_str
                .split('.')
                .next()
                .unwrap_or(&locale_str)
                .replace('_', "-");

            if let Ok(locale) = Locale::try_from_str(&cleaned_locale) {
                return locale;
            }

            // Try language-only fallback (e.g., "en" from "en-US")
            if let Some(lang) = cleaned_locale.split('-').next() {
                if let Ok(locale) = Locale::try_from_str(lang) {
                    return locale;
                }
            }
        }
    }
    tracing::warn!("No valid locale found in environment, using fallback");
    Locale::try_from_str("en-US").expect("Failed to parse fallback locale 'en-US'")
}

pub struct Window {
    core: cosmic::app::Core,
    popup: Option<window::Id>,
    now: Zoned,
    timezone: Option<TimeZone>,
    date_today: Date,
    date_selected: Date,
    rectangle_tracker: Option<RectangleTracker<u32>>,
    rectangle: Rectangle,
    token_tx: Option<calloop::channel::Sender<TokenRequest>>,
    config: TimeAppletConfig,
    show_seconds_tx: watch::Sender<bool>,
    locale: Locale,
    // Calendar state
    calendar_events: BTreeMap<Date, Vec<CalendarEvent>>,
    all_events: Vec<CalendarEvent>,
    upcoming_events: Vec<CalendarEvent>,
    calendar_config: CalendarConfig,
    calendar_config_tx: watch::Sender<CalendarConfig>,
    calendar_syncing: bool,
    calendar_error: Option<String>,
    auth_expired_sources: Vec<String>,
    // Event creation state
    creating_event: bool,
    creating_event_saving: bool,
    new_event_summary: String,
    new_event_start_time: String,
    new_event_end_time: String,
    new_event_all_day: bool,
    new_event_calendar_idx: usize,
    new_event_description: String,
    new_event_location: String,
    // Event detail / editing / deletion state
    viewing_event_uid: Option<String>,
    editing_event: bool,
    editing_event_saving: bool,
    confirming_delete: bool,
    deleting_event: bool,
    // VTODO state
    calendar_todos: Vec<CalendarTodo>,
}

#[derive(Debug, Clone)]
pub enum Message {
    TogglePopup,
    CloseRequested(window::Id),
    Tick,
    Rectangle(RectangleUpdate<u32>),
    SelectDate(Date),
    GoToToday,
    PreviousMonth,
    NextMonth,
    OpenDateTimeSettings,
    Token(TokenUpdate),
    ConfigChanged(TimeAppletConfig),
    TimezoneUpdate(String),
    Surface(surface::Action),
    // Calendar messages
    CalendarSync(Vec<CalendarEvent>, Vec<CalendarTodo>),
    CalendarAuthExpired(Vec<String>),
    CalendarRefresh,
    CalendarConfigChanged(CalendarConfig),
    CalendarToggleSource(String),
    CalendarRemoveSource(String),
    CalendarCtagUpdate(Vec<crate::calendar::config::SourceConfig>),
    // Event creation messages
    CalendarStartCreateEvent,
    CalendarCancelCreateEvent,
    CalendarSubmitCreateEvent,
    CalendarEventCreated,
    CalendarEventCreateError(String),
    CalFormEventSummary(String),
    CalFormEventStartTime(String),
    CalFormEventEndTime(String),
    CalFormEventAllDay(bool),
    CalFormEventCalendar(usize),
    CalFormEventDescription(String),
    CalFormEventLocation(String),
    // Event detail view messages
    CalendarViewEvent(String),
    CalendarCloseEventView,
    CalendarLaunchExternalApp,
    CalendarOpenUrl(String),
    // Event editing messages
    CalendarEditEvent,
    CalendarSaveEditEvent,
    CalendarCancelEditEvent,
    CalendarEventUpdated,
    CalendarEventUpdateError(String),
    // Event deletion messages
    CalendarDeleteEvent,
    CalendarConfirmDeleteEvent,
    CalendarCancelDeleteEvent,
    CalendarEventDeleted,
    CalendarEventDeleteError(String),
    // VTODO messages
    CalendarToggleTodo(String),
    CalendarTodoToggled,
    CalendarTodoError(String),
}

impl Window {
    fn create_datetime(&self, date: &Date) -> DateTime<icu::calendar::Gregorian> {
        DateTime {
            date: IcuDate::try_new_gregorian(
                date.year() as i32,
                date.month() as u8,
                date.day() as u8,
            )
            .unwrap(),
            time: Time::try_new(
                self.now.hour() as u8,
                self.now.minute() as u8,
                self.now.second() as u8,
                0,
            )
            .unwrap(),
        }
    }

    fn calendar_grid(&self) -> Grid<'_, Message> {
        let mut calendar = grid().width(Length::Fill);
        let first_day_of_week = match self.config.first_day_of_week {
            0 => Weekday::Monday,
            1 => Weekday::Tuesday,
            2 => Weekday::Wednesday,
            3 => Weekday::Thursday,
            4 => Weekday::Friday,
            5 => Weekday::Saturday,
            _ => Weekday::Sunday,
        };

        let first_day = get_calendar_first(
            self.date_selected.year(),
            self.date_selected.month(),
            first_day_of_week,
        );

        let prefs = DateTimeFormatterPreferences::from(self.locale.clone());
        let weekday = DateTimeFormatter::try_new(prefs, fieldsets::E::short()).unwrap();

        for i in 0..7 {
            let date = first_day.checked_add(i.days()).unwrap();
            let datetime = self.create_datetime(&date);
            calendar = calendar.push(
                text::caption(weekday.format(&datetime).to_string())
                    .apply(container)
                    .center_x(Length::Fixed(44.0)),
            );
        }
        calendar = calendar.insert_row();

        for i in 0..42 {
            if i > 0 && i % 7 == 0 {
                calendar = calendar.insert_row();
            }

            let date = first_day
                .checked_add(i.days())
                .expect("valid date in calendar range");
            let is_month = date.first_of_month() == self.date_selected.first_of_month();
            let is_day = date == self.date_selected;
            let is_today = date == self.date_today;
            let has_events = self.calendar_events.contains_key(&date);

            calendar = calendar.push(date_button(date, is_month, is_day, is_today, has_events));
        }

        calendar
    }

    fn event_row<'a>(&'a self, ev: &'a CalendarEvent) -> Element<'a, Message> {
        let time_text = text::caption(ev.time_display());
        let summary_text = text::body(&ev.summary);
        let color = parse_hex_color(&ev.color);
        let indicator = container(space::horizontal().width(8).height(8))
            .class(dot_color_class(color))
            .padding([0, 4]);

        menu_button(
            row![indicator, column![time_text, summary_text].spacing(2)]
                .align_y(Alignment::Center)
                .spacing(8),
        )
        .on_press(Message::CalendarViewEvent(ev.uid.clone()))
        .into()
    }

    /// Collect writable CalDAV calendars: (source_id, calendar_href, display_label).
    fn writable_calendars(&self) -> Vec<(String, String, String)> {
        let mut result = Vec::new();
        for source in &self.calendar_config.sources {
            if !source.enabled {
                continue;
            }
            if let SourceType::CalDav { calendars, .. } = &source.source_type {
                for cal in calendars.iter().filter(|c| c.enabled) {
                    let label = format!("{} / {}", source.name, cal.display_name);
                    result.push((source.id.clone(), cal.href.clone(), label));
                }
            }
        }
        result
    }

    fn create_event_form(&self, space_xxs: u16, space_s: u16) -> Element<'_, Message> {
        let writable = self.writable_calendars();

        let mut form = column![].spacing(6).padding([8, 20]);

        // Summary
        form = form.push(
            cosmic::widget::text_input(fl!("calendar-event-summary"), &self.new_event_summary)
                .on_input(Message::CalFormEventSummary),
        );

        // All-day toggle
        form = form.push(
            row![
                cosmic::widget::checkbox(self.new_event_all_day)
                    .label(fl!("calendar-event-all-day"))
                    .on_toggle(Message::CalFormEventAllDay),
            ],
        );

        // Start / End time (only when not all-day)
        if !self.new_event_all_day {
            form = form.push(
                row![
                    cosmic::widget::text_input(fl!("calendar-event-start-time"), &self.new_event_start_time)
                        .on_input(Message::CalFormEventStartTime)
                        .width(Length::FillPortion(1)),
                    cosmic::widget::text_input(fl!("calendar-event-end-time"), &self.new_event_end_time)
                        .on_input(Message::CalFormEventEndTime)
                        .width(Length::FillPortion(1)),
                ]
                .spacing(8),
            );
        }

        // Calendar selector
        if !writable.is_empty() {
            let mut cal_row = row![
                text::caption(fl!("calendar-event-calendar")).apply(container).padding([6, 0]),
            ]
            .spacing(4);

            for (i, (_, _, label)) in writable.iter().enumerate() {
                let class = if i == self.new_event_calendar_idx {
                    button::ButtonClass::Suggested
                } else {
                    button::ButtonClass::Standard
                };
                cal_row = cal_row.push(
                    button::custom(text::caption(label.clone()))
                        .on_press(Message::CalFormEventCalendar(i))
                        .class(class)
                        .padding([4, 8]),
                );
            }
            form = form.push(cal_row);
        }

        // Save / Cancel
        let save_label = if self.creating_event_saving {
            fl!("calendar-event-creating")
        } else {
            fl!("calendar-save")
        };
        let mut save_btn = button::custom(text::body(save_label))
            .class(button::ButtonClass::Suggested)
            .padding([4, 12]);
        if !self.creating_event_saving && !self.new_event_summary.is_empty() && !writable.is_empty()
        {
            save_btn = save_btn.on_press(Message::CalendarSubmitCreateEvent);
        }
        form = form.push(
            row![
                save_btn,
                button::custom(text::body(fl!("calendar-cancel")))
                    .on_press(Message::CalendarCancelCreateEvent)
                    .class(button::ButtonClass::Standard)
                    .padding([4, 12]),
            ]
            .spacing(8),
        );

        let _ = (space_xxs, space_s);
        form.into()
    }

    /// Render the event detail view (and inline edit / delete confirmation).
    fn event_detail_view<'a>(
        &'a self,
        event: &'a CalendarEvent,
        space_xxs: u16,
        space_s: u16,
    ) -> Element<'a, Message> {
        let mut content = column![].padding([8, 0]);

        // ── Header ──
        content = content.push(
            row![
                button::icon(icon::from_name("go-previous-symbolic"))
                    .padding(8)
                    .on_press(Message::CalendarCloseEventView),
                text(fl!("calendar-event-details")).size(16),
                space::horizontal().width(Length::Fill),
            ]
            .align_y(Alignment::Center)
            .padding([12, 20]),
        );

        content = content.push(
            padded_control(divider::horizontal::default()).padding([space_xxs, space_s]),
        );

        // ── Delete confirmation overlay ──
        if self.confirming_delete {
            let label = if self.deleting_event {
                fl!("calendar-event-deleting")
            } else {
                fl!("calendar-event-delete-confirm")
            };
            let mut delete_btn = button::custom(text::body(fl!("calendar-event-delete")))
                .class(button::ButtonClass::Destructive)
                .padding([4, 12]);
            if !self.deleting_event {
                delete_btn = delete_btn.on_press(Message::CalendarConfirmDeleteEvent);
            }
            content = content.push(
                column![
                    text::body(label).apply(container).padding([0, 20]),
                    row![
                        delete_btn,
                        button::custom(text::body(fl!("calendar-cancel")))
                            .on_press(Message::CalendarCancelDeleteEvent)
                            .class(button::ButtonClass::Standard)
                            .padding([4, 12]),
                    ]
                    .spacing(8)
                    .apply(container)
                    .padding([8, 20]),
                ]
                .spacing(8),
            );
            return self
                .core
                .applet
                .popup_container(container(content))
                .into();
        }

        // ── Edit form ──
        if self.editing_event {
            let writable = self.writable_calendars();

            let mut form = column![].spacing(6).padding([8, 20]);

            form = form.push(
                cosmic::widget::text_input(fl!("calendar-event-summary"), &self.new_event_summary)
                    .on_input(Message::CalFormEventSummary),
            );

            form = form.push(
                row![
                    cosmic::widget::checkbox(self.new_event_all_day)
                    .label(fl!("calendar-event-all-day"))
                    .on_toggle(Message::CalFormEventAllDay),
                ],
            );

            if !self.new_event_all_day {
                form = form.push(
                    row![
                        cosmic::widget::text_input(
                            fl!("calendar-event-start-time"),
                            &self.new_event_start_time
                        )
                        .on_input(Message::CalFormEventStartTime)
                        .width(Length::FillPortion(1)),
                        cosmic::widget::text_input(
                            fl!("calendar-event-end-time"),
                            &self.new_event_end_time
                        )
                        .on_input(Message::CalFormEventEndTime)
                        .width(Length::FillPortion(1)),
                    ]
                    .spacing(8),
                );
            }

            form = form.push(
                cosmic::widget::text_input(
                    fl!("calendar-event-description"),
                    &self.new_event_description,
                )
                .on_input(Message::CalFormEventDescription),
            );

            form = form.push(
                cosmic::widget::text_input(
                    fl!("calendar-event-location"),
                    &self.new_event_location,
                )
                .on_input(Message::CalFormEventLocation),
            );

            let save_label = if self.editing_event_saving {
                fl!("calendar-event-updating")
            } else {
                fl!("calendar-save")
            };
            let mut save_btn = button::custom(text::body(save_label))
                .class(button::ButtonClass::Suggested)
                .padding([4, 12]);
            if !self.editing_event_saving
                && !self.new_event_summary.is_empty()
                && !writable.is_empty()
            {
                save_btn = save_btn.on_press(Message::CalendarSaveEditEvent);
            }
            form = form.push(
                row![
                    save_btn,
                    button::custom(text::body(fl!("calendar-cancel")))
                        .on_press(Message::CalendarCancelEditEvent)
                        .class(button::ButtonClass::Standard)
                        .padding([4, 12]),
                ]
                .spacing(8),
            );

            content = content.push(form);
            return self
                .core
                .applet
                .popup_container(container(content))
                .into();
        }

        // ── Read-only detail view ──
        let mut details = column![].spacing(4).padding([8, 20]);

        details = details.push(text::heading(&event.summary));
        details = details.push(text::body(event.time_display()));

        if let Some(desc) = &event.description {
            if !desc.is_empty() {
                details = details.push(space::vertical().height(4));
                details = details.push(text::caption(fl!("calendar-event-description")));
                details = details.push(text::body(desc));
            }
        }
        if let Some(loc) = &event.location {
            if !loc.is_empty() {
                details = details.push(space::vertical().height(4));
                details = details.push(text::caption(fl!("calendar-event-location")));
                details = details.push(text::body(loc));
            }
        }
        if let Some(url) = &event.url {
            if !url.is_empty() {
                details = details.push(space::vertical().height(4));
                details = details.push(text::caption(fl!("calendar-event-url")));
                details = details.push(
                    button::custom(text::body(url))
                        .on_press(Message::CalendarOpenUrl(url.clone()))
                        .class(button::ButtonClass::Link)
                        .padding(0),
                );
            }
        }

        content = content.push(details);

        content = content.push(
            padded_control(divider::horizontal::default()).padding([space_xxs, space_s]),
        );

        // Action buttons
        let mut actions = row![].spacing(8).padding([8, 20]);

        // Edit/Delete only for CalDAV events with etag+href
        if event.etag.is_some() && event.href.is_some() {
            actions = actions.push(
                button::custom(text::body(fl!("calendar-event-edit")))
                    .on_press(Message::CalendarEditEvent)
                    .class(button::ButtonClass::Standard)
                    .padding([4, 12]),
            );
            actions = actions.push(
                button::custom(text::body(fl!("calendar-event-delete")))
                    .on_press(Message::CalendarDeleteEvent)
                    .class(button::ButtonClass::Destructive)
                    .padding([4, 12]),
            );
        }

        actions = actions.push(
            button::custom(text::body(fl!("calendar-event-open-app")))
                .on_press(Message::CalendarLaunchExternalApp)
                .class(button::ButtonClass::Standard)
                .padding([4, 12]),
        );

        content = content.push(actions);

        self.core
            .applet
            .popup_container(container(content))
            .into()
    }

    /// Render the todo section for the popup.
    fn todo_section(&self) -> Element<'_, Message> {
        let mut col = column![].spacing(4);

        col = col.push(
            text::body(fl!("calendar-todos"))
                .apply(container)
                .padding([0, 20]),
        );

        if self.calendar_todos.is_empty() {
            col = col.push(
                text::caption(fl!("calendar-no-todos"))
                    .apply(container)
                    .padding([0, 20]),
            );
        } else {
            for todo in &self.calendar_todos {
                let check_label = if todo.completed { "☑" } else { "☐" };
                let due_text = todo.due_display();
                let color = parse_hex_color(&todo.color);

                let todo_row = row![
                    container(space::horizontal().width(8).height(8))
                        .class(dot_color_class(color))
                        .padding([0, 4]),
                    button::custom(text::body(check_label))
                        .on_press(Message::CalendarToggleTodo(todo.uid.clone()))
                        .class(button::ButtonClass::Text)
                        .padding(4),
                    column![
                        text::body(&todo.summary),
                        text::caption(due_text),
                    ]
                    .spacing(2),
                ]
                .align_y(Alignment::Center)
                .spacing(6);

                col = col.push(todo_row.apply(container).padding([2, 20]));
            }
        }

        col.into()
    }

    fn save_calendar_config(&self) {
        use cosmic_config::CosmicConfigEntry;
        if let Ok(config) = cosmic_config::Config::new(CALENDAR_CONFIG_ID, 1) {
            if let Err(e) = self.calendar_config.write_entry(&config) {
                tracing::error!("Failed to save calendar config: {e}");
            }
        }
        self.calendar_config_tx
            .send_replace(self.calendar_config.clone());
    }

    /// Format with strftime if non-empty and ignore errors.
    ///
    /// Do not use to_string(). The formatter panics on invalid specifiers.
    fn maybe_strftime(&self) -> Option<String> {
        // strftime may override locale specific elements so it stands alone rather
        // than using ICU.
        (!self.config.format_strftime.is_empty())
            .then(|| strtime::format(&self.config.format_strftime, &self.now).ok())
            .flatten()
    }

    fn vertical_layout(&self) -> Element<'_, Message> {
        let elements: Vec<Element<'_, Message>> = if let Some(strftime) = self.maybe_strftime() {
            strftime
                .split_whitespace()
                .map(|piece| self.core.applet.text(piece.to_owned()).into())
                .collect()
        } else {
            let mut elements = Vec::new();
            let date = self.now.date();
            let datetime = self.create_datetime(&date);
            let mut prefs = DateTimeFormatterPreferences::from(self.locale.clone());
            prefs.hour_cycle = Some(if self.config.military_time {
                HourCycle::H23
            } else {
                HourCycle::H12
            });

            if self.config.show_date_in_top_panel {
                let formatted_date = DateTimeFormatter::try_new(prefs, fieldsets::MD::medium())
                    .unwrap()
                    .format(&datetime)
                    .to_string();

                for p in formatted_date.split_whitespace() {
                    elements.push(self.core.applet.text(p.to_owned()).into());
                }
                elements.push(
                    rule::horizontal(2)
                        .width(self.core.applet.suggested_size(true).0)
                        .into(),
                );
            }
            let mut fs = fieldsets::T::medium();
            if !self.config.show_seconds {
                fs = fs.with_time_precision(TimePrecision::Minute);
            }
            let formatted_time = DateTimeFormatter::try_new(prefs, fs)
                .unwrap()
                .format(&datetime)
                .to_string();

            // todo: split using formatToParts when it is implemented
            // https://github.com/unicode-org/icu4x/issues/4936#issuecomment-2128812667
            for p in formatted_time.split_whitespace().flat_map(|s| s.split(':')) {
                elements.push(self.core.applet.text(p.to_owned()).into());
            }

            elements
        };

        let date_time_col = Column::with_children(elements)
            .align_x(Alignment::Center)
            .spacing(4);

        Element::from(
            column!(
                date_time_col,
                space::horizontal().width(Length::Fixed(
                    (self.core.applet.suggested_size(true).0
                        + 2 * self.core.applet.suggested_padding(true).1)
                        as f32
                ))
            )
            .align_x(Alignment::Center),
        )
    }

    fn horizontal_layout(&self) -> Element<'_, Message> {
        let formatted_date = if let Some(strftime) = self.maybe_strftime() {
            strftime
        } else {
            let datetime = self.create_datetime(&self.now.date());
            let mut prefs = DateTimeFormatterPreferences::from(self.locale.clone());
            prefs.hour_cycle = Some(if self.config.military_time {
                HourCycle::H23
            } else {
                HourCycle::H12
            });

            if self.config.show_date_in_top_panel {
                if self.config.show_weekday {
                    let mut fs = fieldsets::MDET::medium();
                    if !self.config.show_seconds {
                        fs = fs.with_time_precision(TimePrecision::Minute);
                    }
                    DateTimeFormatter::try_new(prefs, fs)
                        .unwrap()
                        .format(&datetime)
                        .to_string()
                } else {
                    let mut fs = fieldsets::MDT::medium();
                    if !self.config.show_seconds {
                        fs = fs.with_time_precision(TimePrecision::Minute);
                    }
                    DateTimeFormatter::try_new(prefs, fs)
                        .unwrap()
                        .format(&datetime)
                        .to_string()
                }
            } else {
                let mut fs = fieldsets::T::medium();
                if !self.config.show_seconds {
                    fs = fs.with_time_precision(TimePrecision::Minute);
                }
                DateTimeFormatter::try_new(prefs, fs)
                    .unwrap()
                    .format(&datetime)
                    .to_string()
            }
        };

        Element::from(
            row!(
                self.core.applet.text(formatted_date),
                container(space::vertical().height(Length::Fixed(
                    (self.core.applet.suggested_size(true).1
                        + 2 * self.core.applet.suggested_padding(true).1)
                        as f32
                )))
            )
            .align_y(Alignment::Center),
        )
    }
}

impl cosmic::Application for Window {
    type Message = Message;
    type Executor = cosmic::SingleThreadExecutor;
    type Flags = ();
    const APP_ID: &str = "com.system76.CosmicAppletTime";

    fn init(core: app::Core, _flags: Self::Flags) -> (Self, app::Task<Self::Message>) {
        let locale = get_system_locale();
        let now = Zoned::now();
        let today = now.date();

        let (show_seconds_tx, _) = watch::channel(true);

        let (calendar_config_tx, _) = watch::channel(CalendarConfig::default());

        // Load cached events for fast startup
        let (cached_events, cached_todos) = calendar::cache::load_cache()
            .unwrap_or_default();
        let calendar_events = calendar::events_by_date(&cached_events);
        let upcoming_events = calendar::upcoming_events(
            &cached_events,
            today,
            5, // default upcoming_count before config loads
        );

        (
            Self {
                core,
                popup: None,
                now,
                timezone: None,
                date_today: today,
                date_selected: today,
                rectangle_tracker: None,
                rectangle: Rectangle::default(),
                token_tx: None,
                config: TimeAppletConfig::default(),
                show_seconds_tx,
                locale,
                calendar_events,
                all_events: cached_events,
                upcoming_events,
                calendar_config: CalendarConfig::default(),
                calendar_config_tx,
                calendar_syncing: false,
                calendar_error: None,
                auth_expired_sources: Vec::new(),
                creating_event: false,
                creating_event_saving: false,
                new_event_summary: String::new(),
                new_event_start_time: String::from("09:00"),
                new_event_end_time: String::from("10:00"),
                new_event_all_day: false,
                new_event_calendar_idx: 0,
                new_event_description: String::new(),
                new_event_location: String::new(),
                viewing_event_uid: None,
                editing_event: false,
                editing_event_saving: false,
                confirming_delete: false,
                deleting_event: false,
                calendar_todos: cached_todos,
            },
            Task::none(),
        )
    }

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }

    fn subscription(&self) -> Subscription<Message> {
        fn time_subscription(mut show_seconds: watch::Receiver<bool>) -> Subscription<Message> {
            struct Wrapper {
                inner: watch::Receiver<bool>,
                id: &'static str,
            }
            impl Hash for Wrapper {
                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                    self.id.hash(state);
                }
            }
            Subscription::run_with(
                Wrapper {
                    inner: show_seconds,
                    id: "time-sub",
                },
                |Wrapper { inner, id }| {
                    let mut show_seconds = inner.clone();
                    stream::channel(1, move |mut output: mpsc::Sender<Message>| async move {
                        // Mark this receiver's state as changed so that it always receives an initial
                        // update during the loop below
                        // This allows us to avoid duplicating code from the loop
                        show_seconds.mark_changed();
                        let mut period = 1;
                        let mut timer = time::interval(time::Duration::from_secs(period));
                        timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

                        loop {
                            tokio::select! {
                                    _ = timer.tick() => {
                                        #[cfg(debug_assertions)]
                                        if let Err(err) = output.send(Message::Tick).await {
                                            tracing::error!(?err, "Failed sending tick request to applet");
                                        }
                                        #[cfg(not(debug_assertions))]
                                        let _ = output.send(Message::Tick).await;

                                        // Calculate a delta if we're ticking per minute to keep ticks stable
                                        // Based on i3status-rust
                                        let current = Timestamp::now().as_second() as u64 % period;
                                        if current != 0 {
                                            timer.reset_after(time::Duration::from_secs(period - current));
                                        }
                                    },
                                // Update timer if the user toggles show_seconds
                                Ok(()) = show_seconds.changed() => {
                                    let seconds = *show_seconds.borrow_and_update();
                                    if seconds {
                                        period = 1;
                                        // Subsecond precision isn't needed; skip calculating offset
                                        let period = time::Duration::from_secs(period);
                                        let start = time::Instant::now() + period;
                                        timer = time::interval_at(start, period);
                                    } else {
                                        period = 60;
                                        let delta = time::Duration::from_secs(period - Timestamp::now().as_second() as u64 % period);
                                        let now = time::Instant::now();
                                        // Start ticking from the next minute to update the time properly
                                        let start = now + delta;
                                        let period = time::Duration::from_secs(period);
                                        timer = time::interval_at(start, period);

                                        timer.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
                                    }
                                }
                            }
                        }
                    })
                },
            )
        }

        // Update applet's timezone if the system's timezone changes
        async fn timezone_update(output: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
            let conn = zbus::Connection::system().await?;
            let proxy = TimeDateProxy::new(&conn).await?;

            // The stream always returns the current timezone as its first item even if it wasn't
            // updated. If the proxy is recreated in a loop somehow, the resulting stream will
            // always yield an update immediately which could lead to spammed false updates.
            let mut stream_tz = proxy.receive_timezone_changed().await;
            while let Some(property) = stream_tz.next().await {
                let tz = property.get().await?;
                output
                    .send(Message::TimezoneUpdate(tz))
                    .await
                    .map_err(|e| {
                        zbus::Error::InputOutput(std::sync::Arc::new(std::io::Error::other(e)))
                    })?;
            }
            Ok(())
        }

        fn timezone_subscription() -> Subscription<Message> {
            Subscription::run_with("timezone-sub", |_| {
                stream::channel(1, |mut output| async move {
                    'retry: loop {
                        match timezone_update(&mut output).await {
                            Ok(()) => break 'retry,
                            Err(err) => {
                                tracing::error!(
                                    ?err,
                                    "Automatic timezone updater failed; retrying in one minute"
                                );
                                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                            }
                        }
                    }

                    std::future::pending().await
                })
            })
        }

        // Update the time when waking from sleep
        async fn wake_from_sleep(output: &mut mpsc::Sender<Message>) -> zbus::Result<()> {
            let connection = zbus::Connection::system().await?;
            let proxy = ManagerProxy::new(&connection).await?;

            while let Some(property) = proxy.receive_prepare_for_sleep().await?.next().await {
                let waking = !property.args()?.start();
                if waking {
                    let _ = output.send(Message::Tick).await;
                }
            }
            Ok(())
        }

        fn wake_from_sleep_subscription() -> Subscription<Message> {
            Subscription::run_with("wake-from-suspend-sub", |_| {
                stream::channel(1, |mut output| async move {
                    if let Err(err) = wake_from_sleep(&mut output).await {
                        tracing::error!(?err, "Failed to subscribe to wake-from-sleep signal");
                    }
                })
            })
        }

        fn calendar_sync_subscription(
            config_rx: watch::Receiver<CalendarConfig>,
        ) -> Subscription<Message> {
            struct SyncSub {
                config_rx: watch::Receiver<CalendarConfig>,
                id: &'static str,
            }
            impl Hash for SyncSub {
                fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
                    self.id.hash(state);
                }
            }
            Subscription::run_with(
                SyncSub { config_rx, id: "calendar-sync" },
                |sub| {
                let mut config_rx = sub.config_rx.clone();
                stream::channel(8, move |mut output: mpsc::Sender<Message>| async move {
                    config_rx.mark_changed();

                    let mut interval =
                        time::interval(time::Duration::from_secs(15 * 60));

                    loop {
                        tokio::select! {
                            _ = interval.tick() => {
                                let config = config_rx.borrow().clone();
                                if config.sources.is_empty() { continue; }

                                {
                                    let (result, updated_sources) = calendar::sync_all(&config).await;
                                    let _ = output.send(Message::CalendarSync(result.events, result.todos)).await;
                                    if !result.auth_expired.is_empty() {
                                        let _ = output.send(Message::CalendarAuthExpired(result.auth_expired)).await;
                                    }
                                    if updated_sources != config.sources {
                                        let _ = output.send(Message::CalendarCtagUpdate(updated_sources)).await;
                                    }
                                }
                            }
                            Ok(()) = config_rx.changed() => {
                                let config = config_rx.borrow_and_update().clone();
                                let minutes = config.sync_interval_minutes.max(1);
                                interval = time::interval(
                                    time::Duration::from_secs(u64::from(minutes) * 60),
                                );
                                if config.sources.is_empty() { continue; }
                                {
                                    let (result, updated_sources) = calendar::sync_all(&config).await;
                                    let _ = output.send(Message::CalendarSync(result.events, result.todos)).await;
                                    if !result.auth_expired.is_empty() {
                                        let _ = output.send(Message::CalendarAuthExpired(result.auth_expired)).await;
                                    }
                                    if updated_sources != config.sources {
                                        let _ = output.send(Message::CalendarCtagUpdate(updated_sources)).await;
                                    }
                                }
                            }
                        }
                    }
                })
            })
        }

        let show_seconds_rx = self.show_seconds_tx.subscribe();
        let calendar_config_rx = self.calendar_config_tx.subscribe();
        Subscription::batch([
            rectangle_tracker_subscription(0).map(|e| Message::Rectangle(e.1)),
            time_subscription(show_seconds_rx),
            activation_token_subscription(0).map(Message::Token),
            timezone_subscription(),
            wake_from_sleep_subscription(),
            calendar_sync_subscription(calendar_config_rx),
            self.core.watch_config(Self::APP_ID).map(|u| {
                for err in u.errors {
                    tracing::error!(?err, "Error watching config");
                }
                Message::ConfigChanged(u.config)
            }),
            self.core
                .watch_config::<CalendarConfig>(CALENDAR_CONFIG_ID)
                .map(|u| {
                    for err in u.errors {
                        tracing::error!(?err, "Error watching calendar config");
                    }
                    Message::CalendarConfigChanged(u.config)
                }),
        ])
    }

    fn update(&mut self, message: Self::Message) -> app::Task<Self::Message> {
        match message {
            Message::TogglePopup => {
                if let Some(p) = self.popup.take() {
                    destroy_popup(p)
                } else {
                    self.date_today = self.now.date();
                    self.date_selected = self.date_today;

                    let new_id = window::Id::unique();
                    self.popup = Some(new_id);

                    let mut popup_settings = self.core.applet.get_popup_settings(
                        self.core.main_window_id().unwrap(),
                        new_id,
                        None,
                        None,
                        None,
                    );
                    let Rectangle {
                        x,
                        y,
                        width,
                        height,
                    } = self.rectangle;
                    popup_settings.positioner.anchor_rect = Rectangle::<i32> {
                        x: x.max(1.) as i32,
                        y: y.max(1.) as i32,
                        width: width.max(1.) as i32,
                        height: height.max(1.) as i32,
                    };

                    popup_settings.positioner.size = None;

                    get_popup(popup_settings)
                }
            }
            Message::Tick => {
                self.now = self.timezone.as_ref().map_or_else(
                    || Zoned::now(),
                    |tz| Zoned::now().with_time_zone(tz.clone()),
                );
                Task::none()
            }
            Message::Rectangle(u) => {
                match u {
                    RectangleUpdate::Rectangle(r) => {
                        self.rectangle = r.1;
                    }
                    RectangleUpdate::Init(tracker) => {
                        self.rectangle_tracker = Some(tracker);
                    }
                }
                Task::none()
            }
            Message::CloseRequested(id) => {
                if Some(id) == self.popup {
                    self.popup = None;
                }
                Task::none()
            }
            Message::SelectDate(date) => {
                self.date_selected = date;
                Task::none()
            }
            Message::GoToToday => {
                self.date_selected = self.date_today;
                Task::none()
            }
            Message::PreviousMonth => {
                if let Ok(date) = self.date_selected.checked_sub(1.month()) {
                    self.date_selected = date;
                } else {
                    tracing::error!("invalid date");
                }
                Task::none()
            }
            Message::NextMonth => {
                if let Ok(date) = self.date_selected.checked_add(1.month()) {
                    self.date_selected = date;
                } else {
                    tracing::error!("invalid date");
                }
                Task::none()
            }
            Message::OpenDateTimeSettings => {
                let exec = "cosmic-settings time".to_string();
                if let Some(tx) = self.token_tx.as_ref() {
                    let _ = tx.send(TokenRequest {
                        app_id: Self::APP_ID.to_string(),
                        exec,
                    });
                } else {
                    tracing::error!("Wayland tx is None");
                }
                Task::none()
            }
            Message::Token(u) => {
                match u {
                    TokenUpdate::Init(tx) => {
                        self.token_tx = Some(tx);
                    }
                    TokenUpdate::Finished => {
                        self.token_tx = None;
                    }
                    TokenUpdate::ActivationToken { token, .. } => {
                        let mut cmd = std::process::Command::new("cosmic-settings");
                        cmd.arg("time");
                        if let Some(token) = token {
                            cmd.env("XDG_ACTIVATION_TOKEN", &token);
                            cmd.env("DESKTOP_STARTUP_ID", &token);
                        }
                        tokio::spawn(cosmic::process::spawn(cmd));
                    }
                }
                Task::none()
            }
            Message::ConfigChanged(c) => {
                // Don't interrupt the tick subscription unless necessary
                self.show_seconds_tx.send_if_modified(|show_seconds| {
                    if !c.format_strftime.is_empty() {
                        if c.format_strftime.split('%').any(|s| {
                            STRFTIME_SECONDS.contains(&s.chars().next().unwrap_or_default())
                        }) && !*show_seconds
                        {
                            // The strftime formatter contains a seconds specifier. Force enable
                            // ticking per seconds internally regardless of the user setting.
                            // This does not change the user's setting. It's invisible to the user.
                            *show_seconds = true;
                            true
                        } else {
                            false
                        }
                    } else if *show_seconds == c.show_seconds {
                        false
                    } else {
                        *show_seconds = c.show_seconds;
                        true
                    }
                });
                self.config = c;
                Task::none()
            }
            Message::TimezoneUpdate(timezone) => {
                if let Ok(timezone) = TimeZone::get(&timezone) {
                    self.now = Zoned::now().with_time_zone(timezone.clone());
                    self.date_today = self.now.date();
                    self.date_selected = self.date_today;
                    self.timezone = Some(timezone);
                }

                self.update(Message::Tick)
            }
            Message::Surface(a) => {
                return cosmic::task::message(cosmic::Action::Cosmic(
                    cosmic::app::Action::Surface(a),
                ));
            }
            // ── Calendar message handlers ──────────────────────────
            Message::CalendarSync(events, todos) => {
                self.calendar_events = calendar::events_by_date(&events);
                self.upcoming_events = calendar::upcoming_events(
                    &events,
                    self.date_today,
                    self.calendar_config.upcoming_count,
                );
                calendar::cache::save_cache(&events, &todos);
                self.all_events = events;
                self.calendar_todos = todos;
                self.calendar_syncing = false;
                self.calendar_error = None;
                Task::none()
            }
            Message::CalendarAuthExpired(source_ids) => {
                for sid in &source_ids {
                    if !self.auth_expired_sources.contains(sid) {
                        self.auth_expired_sources.push(sid.clone());
                    }
                }
                Task::none()
            }
            Message::CalendarRefresh => {
                self.calendar_syncing = true;
                self.calendar_config_tx
                    .send_replace(self.calendar_config.clone());
                Task::none()
            }
            Message::CalendarConfigChanged(config) => {
                self.calendar_config = config.clone();
                self.calendar_config_tx.send_replace(config);
                Task::none()
            }
            Message::CalendarToggleSource(id) => {
                if let Some(src) = self.calendar_config.sources.iter_mut().find(|s| s.id == id) {
                    src.enabled = !src.enabled;
                }
                self.save_calendar_config();
                Task::none()
            }
            Message::CalendarRemoveSource(id) => {
                self.calendar_config.sources.retain(|s| s.id != id);
                self.save_calendar_config();
                let source_id = id;
                tokio::spawn(async move {
                    if let Err(e) = calendar::secrets::delete_secrets(&source_id).await {
                        tracing::warn!("Failed to clean up keyring secrets: {e}");
                    }
                });
                Task::none()
            }
            Message::CalendarCtagUpdate(updated_sources) => {
                self.calendar_config.sources = updated_sources;
                self.save_calendar_config();
                Task::none()
            }
            // ── Event creation handlers ────────────────────────────
            Message::CalendarStartCreateEvent => {
                self.creating_event = true;
                self.creating_event_saving = false;
                self.new_event_summary.clear();
                self.new_event_start_time = String::from("09:00");
                self.new_event_end_time = String::from("10:00");
                self.new_event_all_day = false;
                self.new_event_calendar_idx = 0;
                self.new_event_description.clear();
                self.new_event_location.clear();
                Task::none()
            }
            Message::CalendarCancelCreateEvent => {
                self.creating_event = false;
                self.creating_event_saving = false;
                Task::none()
            }
            Message::CalendarSubmitCreateEvent => {
                let writable = self.writable_calendars();
                let Some((source_id, calendar_href, _)) =
                    writable.get(self.new_event_calendar_idx).cloned()
                else {
                    return Task::none();
                };

                let Some(source) = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == source_id)
                else {
                    return Task::none();
                };
                let auth = match &source.source_type {
                    SourceType::CalDav { auth, .. } => auth.clone(),
                    _ => return Task::none(),
                };
                let ca = source.ca_cert_path.clone();

                let date = self.date_selected;
                let summary = self.new_event_summary.clone();
                let all_day = self.new_event_all_day;
                let start_time = self.new_event_start_time.clone();
                let end_time = self.new_event_end_time.clone();

                self.creating_event_saving = true;

                return cosmic::task::future(async move {
                    let uid = uuid::Uuid::new_v4().to_string();

                    let (dtstart, dtend) = if all_day {
                        let start = date
                            .at(0, 0, 0, 0)
                            .to_zoned(jiff::tz::TimeZone::UTC)
                            .unwrap();
                        let end = date
                            .checked_add(jiff::ToSpan::days(1))
                            .unwrap_or(date)
                            .at(0, 0, 0, 0)
                            .to_zoned(jiff::tz::TimeZone::UTC)
                            .unwrap();
                        (start, Some(end))
                    } else {
                        let (sh, sm) = parse_hhmm(&start_time).unwrap_or((9, 0));
                        let (eh, em) = parse_hhmm(&end_time).unwrap_or((10, 0));
                        let start = date
                            .at(sh, sm, 0, 0)
                            .to_zoned(jiff::tz::TimeZone::system())
                            .unwrap();
                        let end = date
                            .at(eh, em, 0, 0)
                            .to_zoned(jiff::tz::TimeZone::system())
                            .unwrap();
                        (start, Some(end))
                    };

                    let event = CalendarEvent {
                        uid,
                        source_id: source_id.clone(),
                        summary,
                        description: None,
                        location: None,
                        dtstart,
                        dtend,
                        all_day,
                        url: None,
                        color: String::new(),
                        etag: None,
                        href: None,
                        rrule: None,
                        exdates: Vec::new(),
                        rdates: Vec::new(),
                    };

                    match calendar::caldav::create_event(
                        &calendar_href,
                        &event,
                        &auth,
                        &source_id,
                        ca.as_deref(),
                    )
                    .await
                    {
                        Ok(()) => Message::CalendarEventCreated,
                        Err(e) => Message::CalendarEventCreateError(e.to_string()),
                    }
                });
            }
            Message::CalendarEventCreated => {
                self.creating_event = false;
                self.creating_event_saving = false;
                // Trigger a sync to pick up the new event from the server
                self.calendar_syncing = true;
                self.calendar_config_tx
                    .send_replace(self.calendar_config.clone());
                Task::none()
            }
            Message::CalendarEventCreateError(err) => {
                tracing::error!("Failed to create event: {err}");
                self.creating_event_saving = false;
                self.calendar_error = Some(err);
                Task::none()
            }
            Message::CalFormEventSummary(v) => {
                self.new_event_summary = v;
                Task::none()
            }
            Message::CalFormEventStartTime(v) => {
                self.new_event_start_time = v;
                Task::none()
            }
            Message::CalFormEventEndTime(v) => {
                self.new_event_end_time = v;
                Task::none()
            }
            Message::CalFormEventAllDay(v) => {
                self.new_event_all_day = v;
                Task::none()
            }
            Message::CalFormEventCalendar(v) => {
                self.new_event_calendar_idx = v;
                Task::none()
            }
            Message::CalFormEventDescription(v) => {
                self.new_event_description = v;
                Task::none()
            }
            Message::CalFormEventLocation(v) => {
                self.new_event_location = v;
                Task::none()
            }
            // ── Event detail view handlers ─────────────────────────
            Message::CalendarViewEvent(uid) => {
                self.viewing_event_uid = Some(uid);
                self.editing_event = false;
                self.confirming_delete = false;
                Task::none()
            }
            Message::CalendarCloseEventView => {
                self.viewing_event_uid = None;
                self.editing_event = false;
                self.editing_event_saving = false;
                self.confirming_delete = false;
                self.deleting_event = false;
                Task::none()
            }
            Message::CalendarLaunchExternalApp => {
                let calendar_app = if self.calendar_config.calendar_app.is_empty() {
                    "gnome-calendar".to_string()
                } else {
                    self.calendar_config.calendar_app.clone()
                };
                if let Err(e) = std::process::Command::new(&calendar_app).spawn() {
                    tracing::error!("Failed to open calendar app '{calendar_app}': {e}");
                }
                Task::none()
            }
            Message::CalendarOpenUrl(url) => {
                if let Err(e) = std::process::Command::new("xdg-open").arg(&url).spawn() {
                    tracing::error!("Failed to open URL '{url}': {e}");
                }
                Task::none()
            }
            // ── Event editing handlers ─────────────────────────────
            Message::CalendarEditEvent => {
                if let Some(ref uid) = self.viewing_event_uid {
                    if let Some(event) = self.all_events.iter().find(|e| e.uid == *uid) {
                        self.new_event_summary = event.summary.clone();
                        self.new_event_description =
                            event.description.clone().unwrap_or_default();
                        self.new_event_location = event.location.clone().unwrap_or_default();
                        self.new_event_all_day = event.all_day;
                        self.new_event_start_time =
                            format!("{:02}:{:02}", event.dtstart.hour(), event.dtstart.minute());
                        self.new_event_end_time = event
                            .dtend
                            .as_ref()
                            .map(|e| format!("{:02}:{:02}", e.hour(), e.minute()))
                            .unwrap_or_else(|| "10:00".to_string());
                        self.editing_event = true;
                        self.editing_event_saving = false;
                    }
                }
                Task::none()
            }
            Message::CalendarCancelEditEvent => {
                self.editing_event = false;
                self.editing_event_saving = false;
                Task::none()
            }
            Message::CalendarSaveEditEvent => {
                let Some(ref uid) = self.viewing_event_uid else {
                    return Task::none();
                };
                let Some(event) = self.all_events.iter().find(|e| e.uid == *uid).cloned() else {
                    return Task::none();
                };
                let (Some(href), Some(etag)) = (event.href.clone(), event.etag.clone()) else {
                    return Task::none();
                };

                let source = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == event.source_id);
                let auth = match source.map(|s| &s.source_type) {
                    Some(SourceType::CalDav { auth, .. }) => auth.clone(),
                    _ => return Task::none(),
                };
                let ca = source.and_then(|s| s.ca_cert_path.clone());

                let summary = self.new_event_summary.clone();
                let description = if self.new_event_description.is_empty() {
                    None
                } else {
                    Some(self.new_event_description.clone())
                };
                let location = if self.new_event_location.is_empty() {
                    None
                } else {
                    Some(self.new_event_location.clone())
                };
                let all_day = self.new_event_all_day;
                let start_time = self.new_event_start_time.clone();
                let end_time = self.new_event_end_time.clone();
                let source_id = event.source_id.clone();

                self.editing_event_saving = true;

                return cosmic::task::future(async move {
                    let date = event.dtstart.date();
                    let (dtstart, dtend) = if all_day {
                        let start = date
                            .at(0, 0, 0, 0)
                            .to_zoned(jiff::tz::TimeZone::UTC)
                            .unwrap();
                        let end = date
                            .checked_add(jiff::ToSpan::days(1))
                            .unwrap_or(date)
                            .at(0, 0, 0, 0)
                            .to_zoned(jiff::tz::TimeZone::UTC)
                            .unwrap();
                        (start, Some(end))
                    } else {
                        let (sh, sm) = parse_hhmm(&start_time).unwrap_or((9, 0));
                        let (eh, em) = parse_hhmm(&end_time).unwrap_or((10, 0));
                        let start = date
                            .at(sh, sm, 0, 0)
                            .to_zoned(jiff::tz::TimeZone::system())
                            .unwrap();
                        let end = date
                            .at(eh, em, 0, 0)
                            .to_zoned(jiff::tz::TimeZone::system())
                            .unwrap();
                        (start, Some(end))
                    };

                    let updated = CalendarEvent {
                        summary,
                        description,
                        location,
                        dtstart,
                        dtend,
                        all_day,
                        ..event
                    };

                    match calendar::caldav::update_event(&href, &updated, &etag, &auth, &source_id, ca.as_deref())
                        .await
                    {
                        Ok(()) => Message::CalendarEventUpdated,
                        Err(e) => Message::CalendarEventUpdateError(e.to_string()),
                    }
                });
            }
            Message::CalendarEventUpdated => {
                self.editing_event = false;
                self.editing_event_saving = false;
                self.viewing_event_uid = None;
                self.calendar_syncing = true;
                self.calendar_config_tx
                    .send_replace(self.calendar_config.clone());
                Task::none()
            }
            Message::CalendarEventUpdateError(err) => {
                tracing::error!("Failed to update event: {err}");
                self.editing_event_saving = false;
                self.calendar_error = Some(err);
                Task::none()
            }
            // ── Event deletion handlers ────────────────────────────
            Message::CalendarDeleteEvent => {
                self.confirming_delete = true;
                Task::none()
            }
            Message::CalendarCancelDeleteEvent => {
                self.confirming_delete = false;
                Task::none()
            }
            Message::CalendarConfirmDeleteEvent => {
                let Some(ref uid) = self.viewing_event_uid else {
                    return Task::none();
                };
                let Some(event) = self.all_events.iter().find(|e| e.uid == *uid).cloned() else {
                    return Task::none();
                };
                let (Some(href), Some(etag)) = (event.href.clone(), event.etag.clone()) else {
                    return Task::none();
                };

                let source = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == event.source_id);
                let auth = match source.map(|s| &s.source_type) {
                    Some(SourceType::CalDav { auth, .. }) => auth.clone(),
                    _ => return Task::none(),
                };
                let ca = source.and_then(|s| s.ca_cert_path.clone());

                let source_id = event.source_id.clone();
                self.deleting_event = true;

                return cosmic::task::future(async move {
                    match calendar::caldav::delete_event(&href, &etag, &auth, &source_id, ca.as_deref()).await {
                        Ok(()) => Message::CalendarEventDeleted,
                        Err(e) => Message::CalendarEventDeleteError(e.to_string()),
                    }
                });
            }
            Message::CalendarEventDeleted => {
                self.viewing_event_uid = None;
                self.confirming_delete = false;
                self.deleting_event = false;
                self.calendar_syncing = true;
                self.calendar_config_tx
                    .send_replace(self.calendar_config.clone());
                Task::none()
            }
            Message::CalendarEventDeleteError(err) => {
                tracing::error!("Failed to delete event: {err}");
                self.deleting_event = false;
                self.confirming_delete = false;
                self.calendar_error = Some(err);
                Task::none()
            }
            // ── VTODO handlers ─────────────────────────────────────
            Message::CalendarToggleTodo(uid) => {
                let Some(todo) = self.calendar_todos.iter().find(|t| t.uid == uid).cloned()
                else {
                    return Task::none();
                };
                let (Some(href), Some(etag)) = (todo.href.clone(), todo.etag.clone()) else {
                    return Task::none();
                };

                let source = self
                    .calendar_config
                    .sources
                    .iter()
                    .find(|s| s.id == todo.source_id);
                let auth = match source.map(|s| &s.source_type) {
                    Some(SourceType::CalDav { auth, .. }) => auth.clone(),
                    _ => return Task::none(),
                };
                let ca = source.and_then(|s| s.ca_cert_path.clone());

                let source_id = todo.source_id.clone();

                return cosmic::task::future(async move {
                    match calendar::caldav::complete_todo(&href, &todo, &etag, &auth, &source_id, ca.as_deref())
                        .await
                    {
                        Ok(()) => Message::CalendarTodoToggled,
                        Err(e) => Message::CalendarTodoError(e.to_string()),
                    }
                });
            }
            Message::CalendarTodoToggled => {
                self.calendar_syncing = true;
                self.calendar_config_tx
                    .send_replace(self.calendar_config.clone());
                Task::none()
            }
            Message::CalendarTodoError(err) => {
                tracing::error!("Failed to toggle todo: {err}");
                self.calendar_error = Some(err);
                Task::none()
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let horizontal = matches!(
            self.core.applet.anchor,
            PanelAnchor::Top | PanelAnchor::Bottom
        );

        let button = button::custom(if horizontal {
            self.horizontal_layout()
        } else {
            self.vertical_layout()
        })
        .padding(if horizontal {
            [0, self.core.applet.suggested_padding(true).0]
        } else {
            [self.core.applet.suggested_padding(true).0, 0]
        })
        .on_press_down(Message::TogglePopup)
        .class(cosmic::theme::Button::AppletIcon);

        autosize::autosize(
            if let Some(tracker) = self.rectangle_tracker.as_ref() {
                Element::from(tracker.container(0, button).ignore_bounds(true))
            } else {
                button.into()
            },
            AUTOSIZE_MAIN_ID.clone(),
        )
        .into()
    }

    fn view_window(&self, _id: window::Id) -> Element<'_, Message> {
        let Spacing {
            space_xxs, space_s, ..
        } = theme::active().cosmic().spacing;

        // Event detail view
        if let Some(ref uid) = self.viewing_event_uid {
            if let Some(event) = self.all_events.iter().find(|e| e.uid == *uid) {
                return self.event_detail_view(event, space_xxs, space_s);
            }
        }

        let datetime = self.create_datetime(&self.date_selected);
        let prefs = DateTimeFormatterPreferences::from(self.locale.clone());

        let date = text(
            DateTimeFormatter::try_new(prefs, fieldsets::YMD::long())
                .unwrap()
                .format(&datetime)
                .to_string(),
        )
        .size(18);
        let day_of_week = text::body(
            DateTimeFormatter::try_new(prefs, fieldsets::E::long())
                .unwrap()
                .format(&datetime)
                .to_string(),
        );

        let month_controls = row![
            button::icon(icon::from_name("go-previous-symbolic"))
                .padding(8)
                .on_press(Message::PreviousMonth),
            button::icon(icon::from_name("go-next-symbolic"))
                .padding(8)
                .on_press(Message::NextMonth)
        ]
        .spacing(8);

        let mut header_buttons = row![].spacing(8).align_y(Alignment::Center);
        if self.date_selected != self.date_today {
            header_buttons = header_buttons.push(
                button::text(fl!("calendar-today"))
                    .class(button::ButtonClass::Standard)
                    .on_press(Message::GoToToday),
            );
        }
        if !self.writable_calendars().is_empty() {
            header_buttons = header_buttons.push(
                button::icon(icon::from_name("list-add-symbolic"))
                    .padding(8)
                    .on_press(Message::CalendarStartCreateEvent),
            );
        }
        header_buttons = header_buttons.push(month_controls);

        let calendar = self.calendar_grid();

        let mut content_list = column![
            row![
                column![date, day_of_week],
                space::horizontal().width(Length::Fill),
                header_buttons,
            ]
            .align_y(Alignment::Center)
            .padding([12, 20]),
            calendar.padding([0, 12].into()),
        ]
        .padding([8, 0]);

        // Sync status indicator
        if self.calendar_syncing {
            content_list = content_list.push(
                row![
                    icon::from_name("emblem-synchronizing-symbolic").size(16),
                    text::body(fl!("calendar-syncing")),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .apply(container)
                .padding([4, 20]),
            );
        }

        // Error banner
        if let Some(ref err) = self.calendar_error {
            content_list = content_list.push(
                row![
                    icon::from_name("dialog-warning-symbolic").size(16),
                    text::body(format!("{}: {}", fl!("calendar-sync-error"), err)),
                ]
                .spacing(8)
                .align_y(Alignment::Center)
                .apply(container)
                .padding([4, 20]),
            );
        }

        // Auth-expired warning
        if !self.auth_expired_sources.is_empty() {
            let source_names: Vec<&str> = self.auth_expired_sources.iter().filter_map(|sid| {
                self.calendar_config.sources.iter().find(|s| s.id == *sid).map(|s| s.name.as_str())
            }).collect();
            if !source_names.is_empty() {
                content_list = content_list.push(
                    row![
                        icon::from_name("dialog-password-symbolic").size(16),
                        text::body(format!("{}: {}", fl!("calendar-source-oidc-reauth"), source_names.join(", "))),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .apply(container)
                    .padding([4, 20]),
                );
            }
        }

        // Events for the selected day
        let day_events = self.calendar_events.get(&self.date_selected);
        if let Some(events) = day_events {
            if !events.is_empty() {
                content_list = content_list
                    .push(padded_control(divider::horizontal::default()).padding([space_xxs, space_s]));
                content_list = content_list.push(
                    text::body(fl!("calendar-events-today"))
                        .apply(container)
                        .padding([0, 20]),
                );
                for ev in events {
                    content_list = content_list.push(self.event_row(ev));
                }
            }
        }

        // Add Event form
        if self.creating_event {
            content_list = content_list
                .push(padded_control(divider::horizontal::default()).padding([space_xxs, space_s]));
            content_list = content_list.push(self.create_event_form(space_xxs, space_s));
        }

        // Upcoming events
        if !self.upcoming_events.is_empty() {
            content_list = content_list
                .push(padded_control(divider::horizontal::default()).padding([space_xxs, space_s]));
            content_list = content_list.push(
                text::body(fl!("calendar-upcoming"))
                    .apply(container)
                    .padding([0, 20]),
            );
            for ev in &self.upcoming_events {
                content_list = content_list.push(self.event_row(ev));
            }
        }

        // Todos section
        if !self.calendar_todos.is_empty() {
            content_list = content_list.push(
                padded_control(divider::horizontal::default()).padding([space_xxs, space_s]),
            );
            content_list = content_list.push(self.todo_section());
        }

        content_list = content_list
            .push(padded_control(divider::horizontal::default()).padding([space_xxs, space_s]));

        content_list = content_list.push(
            menu_button(text::body(fl!("datetime-settings")))
                .on_press(Message::OpenDateTimeSettings),
        );

        self.core
            .applet
            .popup_container(container(content_list))
            .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        Some(Message::CloseRequested(id))
    }
}

fn date_button(date: Date, is_month: bool, is_day: bool, is_today: bool, has_events: bool) -> Button<'static, Message> {
    let day = date.day();
    let style = if is_day {
        button::ButtonClass::Suggested
    } else if is_today {
        button::ButtonClass::Standard
    } else {
        button::ButtonClass::Text
    };

    let content: Element<'static, Message> = if has_events {
        column![
            text::body(format!("{day}"))
                .apply(container)
                .center_x(Length::Fill),
            text::caption("●")
                .apply(container)
                .center_x(Length::Fill),
        ]
        .align_x(Alignment::Center)
        .into()
    } else {
        text::body(format!("{day}"))
            .apply(container)
            .center(Length::Fill)
            .into()
    };

    let btn = button::custom(content)
        .class(style)
        .height(Length::Fixed(44.0))
        .width(Length::Fixed(44.0));

    if is_month || is_day || is_today {
        btn.on_press(Message::SelectDate(date))
    } else {
        btn
    }
}

fn parse_hex_color(hex: &str) -> cosmic::iced::Color {
    let hex = hex.trim_start_matches('#');
    if hex.len() >= 6 {
        let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(128);
        let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(128);
        let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(128);
        cosmic::iced::Color::from_rgb8(r, g, b)
    } else {
        cosmic::iced::Color::from_rgb8(128, 128, 128)
    }
}

fn dot_color_class(color: cosmic::iced::Color) -> cosmic::theme::Container<'static> {
    cosmic::theme::Container::Custom(Box::new(move |_theme| {
        cosmic::widget::container::Style {
            icon_color: None,
            text_color: None,
            background: Some(cosmic::iced::Background::Color(color)),
            border: cosmic::iced_core::Border {
                radius: 4.0.into(),
                width: 0.0,
                color: cosmic::iced::Color::TRANSPARENT,
            },
            shadow: Default::default(),
            snap: Default::default(),
        }
    }))
}

/// Parse a "HH:MM" string into (hour, minute).
fn parse_hhmm(s: &str) -> Option<(i8, i8)> {
    let mut parts = s.split(':');
    let h: i8 = parts.next()?.trim().parse().ok()?;
    let m: i8 = parts.next()?.trim().parse().ok()?;
    if (0..24).contains(&h) && (0..60).contains(&m) {
        Some((h, m))
    } else {
        None
    }
}
