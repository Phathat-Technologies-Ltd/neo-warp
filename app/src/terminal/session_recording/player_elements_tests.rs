use super::*;

#[test]
fn recording_screen_shrinks_to_fit_width() {
    // 80 columns measured 800px wide at 13pt; 400px available halves the font.
    let size = fitted_font_size(13., vec2f(800., 300.), vec2f(400., 1000.), 4., 20.);
    assert_eq!(size, 6.5);
}

#[test]
fn recording_screen_fits_the_tighter_dimension() {
    let size = fitted_font_size(10., vec2f(500., 500.), vec2f(1000., 250.), 4., 20.);
    assert_eq!(size, 5.);
}

#[test]
fn recording_screen_scale_is_clamped() {
    assert_eq!(
        fitted_font_size(13., vec2f(100., 100.), vec2f(10_000., 10_000.), 4., 20.),
        20.
    );
    assert_eq!(
        fitted_font_size(13., vec2f(10_000., 100.), vec2f(100., 100.), 4., 20.),
        4.
    );
    assert_eq!(
        fitted_font_size(
            13.,
            vec2f(100., 100.),
            vec2f(f32::INFINITY, f32::INFINITY),
            4.,
            20.
        ),
        13.
    );
}
