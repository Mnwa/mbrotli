//! Public API behaviour of [`Window`], its two headers and their bounds.

use mbrotli::{Compressor, ConfigError, EncoderConfig, Quality, Window, WindowEncoding};

#[test]
fn accepts_every_window_size_the_ordinary_header_allows() {
    for lgwin in 10u8..=24 {
        let window = Window::standard(lgwin).expect("window size is within bounds");

        assert_eq!(window.bits(), lgwin);
        assert_eq!(window.encoding(), WindowEncoding::Standard);
    }
}

#[test]
fn accepts_every_window_size_the_large_header_allows() {
    for lgwin in 10u8..=62 {
        let window = Window::large(lgwin).expect("window size is within bounds");

        assert_eq!(window.bits(), lgwin);
        assert_eq!(window.encoding(), WindowEncoding::Large);
    }
}

#[test]
fn the_same_size_asked_for_two_ways_is_two_windows() {
    for lgwin in 10u8..=24 {
        let ordinary = Window::standard(lgwin).expect("within bounds");
        let large = Window::large(lgwin).expect("within bounds");

        assert_ne!(ordinary, large, "{lgwin} bits");
        assert_eq!(ordinary.bits(), large.bits());
        assert_ne!(ordinary.encoding(), large.encoding());
    }
}

#[test]
fn rejects_window_sizes_below_the_lower_bound() {
    for lgwin in 0u8..10 {
        assert_eq!(
            Window::standard(lgwin),
            Err(ConfigError::StandardWindow { requested: lgwin })
        );
        assert_eq!(
            Window::large(lgwin),
            Err(ConfigError::LargeWindow { requested: lgwin })
        );
    }
}

#[test]
fn rejects_window_sizes_above_each_headers_ceiling() {
    // Twenty-five is past the ordinary header but well inside the large one.
    assert_eq!(
        Window::standard(25),
        Err(ConfigError::StandardWindow { requested: 25 })
    );
    assert!(Window::large(25).is_ok());

    assert_eq!(
        Window::standard(u8::MAX),
        Err(ConfigError::StandardWindow { requested: u8::MAX })
    );
    assert_eq!(
        Window::large(63),
        Err(ConfigError::LargeWindow { requested: 63 })
    );
    assert_eq!(
        Window::large(u8::MAX),
        Err(ConfigError::LargeWindow { requested: u8::MAX })
    );
}

#[test]
fn the_named_bounds_are_the_ones_each_header_allows() {
    assert_eq!(Window::MIN_BITS, 10);
    assert_eq!(Window::MAX_STANDARD_BITS, 24);
    assert_eq!(Window::MAX_LARGE_BITS, 62);
    assert_eq!(
        Window::DEFAULT,
        Window::standard(22).expect("within bounds")
    );
    assert!(Window::standard(Window::MIN_BITS).is_ok());
    assert!(Window::standard(Window::MAX_STANDARD_BITS).is_ok());
    assert!(Window::large(Window::MAX_LARGE_BITS).is_ok());
}

#[test]
fn defaults_to_the_brotli_default_window() {
    assert_eq!(Window::default(), Window::DEFAULT);
    assert_eq!(Window::default().bits(), 22);
    assert_eq!(Window::default().encoding(), WindowEncoding::Standard);
}

#[test]
fn error_messages_name_the_bound_that_was_missed() {
    assert!(
        ConfigError::StandardWindow { requested: 9 }
            .to_string()
            .contains("10..=24")
    );
    assert!(
        ConfigError::LargeWindow { requested: 63 }
            .to_string()
            .contains("10..=62")
    );
}

#[test]
fn a_configuration_reports_the_window_it_was_built_with() {
    let ordinary = Window::standard(18).expect("18 is within bounds");
    let config = EncoderConfig::default()
        .with_quality(Quality::Q0)
        .with_window(ordinary);

    assert_eq!(config.window(), ordinary);
    assert_eq!(config.window().bits(), 18);

    let large = Window::large(40).expect("40 is within bounds");
    let config = EncoderConfig::default()
        .with_quality(Quality::Q5)
        .with_window(large);

    assert_eq!(config.window(), large);
    assert_eq!(config.window().encoding(), WindowEncoding::Large);
}

#[test]
fn a_large_window_is_refused_only_where_the_distance_model_cannot_carry_one() {
    let large = Window::large(30).expect("30 is within bounds");
    for value in 0u8..=11 {
        let quality = Quality::try_from(value).expect("a legal quality");
        let config = EncoderConfig::default()
            .with_quality(quality)
            .with_window(large);
        let outcome = Compressor::new(config);
        if value <= 2 {
            assert_eq!(
                outcome.err(),
                Some(ConfigError::LargeWindowUnsupportedForQuality { quality }),
                "quality {value} accepted a large window"
            );
        } else {
            assert!(outcome.is_ok(), "quality {value} refused a large window");
        }
    }
}
