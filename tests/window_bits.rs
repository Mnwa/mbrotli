//! Public API behavior of [`WindowBits`] and its bounds.

use mbrotli::compressor::{CompressParams, ParseWindowBitsError, QualityLevel, WindowBits};

#[test]
fn accepts_every_window_size_the_format_allows() {
    for lgwin in usize::from(WindowBits::MIN)..=usize::from(WindowBits::MAX) {
        let bits = WindowBits::try_from(lgwin).expect("window size is within bounds");

        assert_eq!(usize::from(bits), lgwin);
    }
}

#[test]
fn rejects_window_sizes_below_the_lower_bound() {
    let error = WindowBits::try_from(9).expect_err("9 is below the lower bound");

    assert!(matches!(error, ParseWindowBitsError::LowerBound));
    assert_eq!(
        error.to_string(),
        "Window bits should be greater than or equal to 10"
    );
}

#[test]
fn rejects_window_sizes_above_the_upper_bound() {
    let error = WindowBits::try_from(25).expect_err("25 is above the upper bound");

    assert!(matches!(error, ParseWindowBitsError::UpperBound));
    assert_eq!(
        error.to_string(),
        "Window bits should be less than or equal to 24"
    );
}

#[test]
fn rejects_a_zero_window_size() {
    let error = WindowBits::try_from(0).expect_err("0 is below the lower bound");

    assert!(matches!(error, ParseWindowBitsError::LowerBound));
}

#[test]
fn converts_between_a_window_size_and_its_logarithm() {
    let bits = WindowBits::try_from(16).expect("16 is within bounds");

    assert_eq!(usize::from(bits), 16);
    assert_eq!(bits, WindowBits::try_from(16).expect("16 is within bounds"));
}

#[test]
fn defaults_to_the_brotli_default_window() {
    assert_eq!(WindowBits::default(), WindowBits::DEFAULT);
    assert_eq!(usize::from(WindowBits::default()), 22);
}

#[test]
fn orders_window_sizes_by_their_logarithm() {
    assert!(WindowBits::MIN < WindowBits::DEFAULT);
    assert!(WindowBits::DEFAULT < WindowBits::MAX);
}

#[test]
fn params_report_the_window_size_they_were_built_with() {
    let lgwin = WindowBits::try_from(18).expect("18 is within bounds");
    let params = CompressParams::new(QualityLevel::Q1, lgwin);

    assert_eq!(params.lgwin(), lgwin);
    assert_eq!(usize::from(params.quality()), 1);
}
