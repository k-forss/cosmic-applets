use crate::calendar::config::{
    AuthMethod, CalDavCalendar, CalendarConfig, SourceConfig, SourceType,
};

// deserialize_color_compat with current format
#[test]
fn color_compat_current_format() {
    let json = r##"{"href":"h","display_name":"Cal","color":"#FF0000","enabled":true}"##;
    let cal: CalDavCalendar = serde_json::from_str(json).unwrap();
    assert_eq!(cal.color, "#FF0000");
}

// deserialize_color_compat with null
#[test]
fn color_compat_null() {
    let json = r##"{"href":"h","display_name":"Cal","color":null,"enabled":true}"##;
    let cal: CalDavCalendar = serde_json::from_str(json).unwrap();
    assert_eq!(cal.color, "#0078D4");
}

// full CalendarConfig round-trip
#[test]
fn full_config_serde() {
    let config = CalendarConfig {
        sources: vec![
            SourceConfig::new(
                "CalDAV".into(),
                "#FF0000".into(),
                SourceType::CalDav {
                    url: "https://dav.example.com".into(),
                    auth: AuthMethod::Basic {
                        username: "user".into(),
                    },
                    calendars: vec![CalDavCalendar {
                        href: "/cal/1".into(),
                        display_name: "Personal".into(),
                        color: "#00FF00".into(),
                        enabled: true,
                        ctag: None,
                        sync_token: None,
                    }],
                },
            ),
            SourceConfig::new(
                "ICS URL".into(),
                "#0000FF".into(),
                SourceType::IcsUrl {
                    url: "https://example.com/cal.ics".into(),
                    auth: AuthMethod::None,
                },
            ),
            SourceConfig::new(
                "ICS File".into(),
                "#FFFF00".into(),
                SourceType::IcsFile {
                    path: "/tmp/test.ics".into(),
                },
            ),
        ],
        ..CalendarConfig::default()
    };
    let json = serde_json::to_string(&config).unwrap();
    let deser: CalendarConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deser.sources.len(), 3);
    assert_eq!(deser.sources[0].name, "CalDAV");
    match &deser.sources[0].source_type {
        SourceType::CalDav { url, calendars, .. } => {
            assert_eq!(url, "https://dav.example.com");
            assert_eq!(calendars.len(), 1);
        }
        _ => panic!("expected CalDav source type"),
    }
}

// ctag/sync_token not in serialized output
#[test]
fn ctag_sync_token_skip_serializing() {
    let cal = CalDavCalendar {
        href: "/cal".into(),
        display_name: "Test".into(),
        color: "#000".into(),
        enabled: true,
        ctag: Some("ctag-123".into()),
        sync_token: Some("token-456".into()),
    };
    let json = serde_json::to_string(&cal).unwrap();
    assert!(!json.contains("ctag"), "ctag should not be serialized: {json}");
    assert!(!json.contains("sync_token"), "sync_token should not be serialized: {json}");
}
