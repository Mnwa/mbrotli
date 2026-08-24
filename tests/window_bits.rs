//! Public API behavior of [`WindowBits`], its two headers and their bounds.

use mbrotli::compressor::{CompressParams, ParseWindowBitsError, QualityLevel, WindowBits};

#[test]
fn accepts_every_window_size_the_ordinary_header_allows() {
    for lgwin in 10u8..=24 {
        let bits = WindowBits::standard(lgwin).expect("window size is within bounds");

        assert_eq!(bits.bits(), lgwin);
        assert_eq!(usize::from(bits), usize::from(lgwin));
        assert!(!bits.is_large());
    }
}

#[test]
fn accepts_every_window_size_the_large_header_allows() {
    for lgwin in 10u8..=62 {
        let bits = WindowBits::large(lgwin).expect("window size is within bounds");

        assert_eq!(bits.bits(), lgwin);
        assert!(bits.is_large());
    }
}

#[test]
fn the_same_size_asked_for_two_ways_is_two_windows() {
    for lgwin in 10u8..=24 {
        let ordinary = WindowBits::standard(lgwin).expect("within bounds");
        let large = WindowBits::large(lgwin).expect("within bounds");

        assert_ne!(ordinary, large, "{lgwin} bits");
        assert_eq!(ordinary.bits(), large.bits());
    }
}

#[test]
fn rejects_window_sizes_below_the_lower_bound() {
    for lgwin in 0u8..10 {
        assert!(matches!(
            WindowBits::standard(lgwin),
            Err(ParseWindowBitsError::LowerBound)
        ));
        assert!(matches!(
            WindowBits::large(lgwin),
            Err(ParseWindowBitsError::LowerBound)
        ));
    }
}

#[test]
fn rejects_window_sizes_above_each_headers_ceiling() {
    // Twenty-five is past the ordinary header but well inside the large one.
    assert!(matches!(
        WindowBits::standard(25),
        Err(ParseWindowBitsError::UpperBound)
    ));
    assert!(WindowBits::large(25).is_ok());

    assert!(matches!(
        WindowBits::standard(u8::MAX),
        Err(ParseWindowBitsError::UpperBound)
    ));
    assert!(matches!(
        WindowBits::large(63),
        Err(ParseWindowBitsError::LargeUpperBound)
    ));
    assert!(matches!(
        WindowBits::large(u8::MAX),
        Err(ParseWindowBitsError::LargeUpperBound)
    ));
}

#[test]
fn the_named_bounds_are_the_ones_each_header_allows() {
    assert_eq!(
        WindowBits::MIN,
        WindowBits::standard(10).expect("within bounds")
    );
    assert_eq!(
        WindowBits::MAX,
        WindowBits::standard(24).expect("within bounds")
    );
    assert_eq!(
        WindowBits::LARGE_MIN,
        WindowBits::large(10).expect("within bounds")
    );
    assert_eq!(
        WindowBits::LARGE_MAX,
        WindowBits::large(62).expect("within bounds")
    );
    assert_eq!(
        WindowBits::DEFAULT,
        WindowBits::standard(22).expect("within bounds")
    );
}

#[test]
fn defaults_to_the_brotli_default_window() {
    assert_eq!(WindowBits::default(), WindowBits::DEFAULT);
    assert_eq!(usize::from(WindowBits::default()), 22);
    assert!(!WindowBits::default().is_large());
}

#[test]
fn error_messages_name_the_bound_that_was_missed() {
    assert_eq!(
        ParseWindowBitsError::LowerBound.to_string(),
        "Window bits should be greater than or equal to 10"
    );
    assert_eq!(
        ParseWindowBitsError::UpperBound.to_string(),
        "Window bits should be less than or equal to 24"
    );
    assert_eq!(
        ParseWindowBitsError::LargeUpperBound.to_string(),
        "Large window bits should be less than or equal to 62"
    );
}

#[test]
fn params_report_the_window_size_they_were_built_with() {
    let lgwin = WindowBits::standard(18).expect("18 is within bounds");
    let params = CompressParams::new(QualityLevel::Q0, lgwin);

    assert_eq!(params.lgwin(), lgwin);
    assert_eq!(usize::from(params.lgwin()), 18);

    let large = WindowBits::large(40).expect("40 is within bounds");
    let params = CompressParams::new(QualityLevel::Q5, large);

    assert_eq!(params.lgwin(), large);
    assert!(params.lgwin().is_large());
}
