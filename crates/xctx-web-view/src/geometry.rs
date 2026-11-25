//! From GPUI's window coordinates to the frame of a subview of GPUI's native view.

/// A rectangle in points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// The left edge.
    pub x: f64,
    /// The top edge in GPUI's coordinates (top-left origin); the bottom edge in an unflipped `NSView`'s.
    pub y: f64,
    /// The width, never negative.
    pub width: f64,
    /// The height, never negative.
    pub height: f64,
}

/// The frame, in the coordinates of the host view (GPUI's native `NSView`, `host_height` points tall), of a
/// rectangle given in GPUI's window coordinates (logical pixels, top-left origin). A `flipped` host has its
/// origin at the top left too, so nothing moves; an unflipped one (AppKit's default, and GPUI's) counts `y`
/// up from its bottom edge. Negative sizes become zero.
pub fn host_frame(bounds: Frame, host_height: f64, flipped: bool) -> Frame {
    let width = bounds.width.max(0.0);
    let height = bounds.height.max(0.0);
    let y = if flipped {
        bounds.y
    } else {
        host_height - bounds.y - height
    };
    Frame {
        x: bounds.x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(x: f64, y: f64, width: f64, height: f64) -> Frame {
        Frame {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn an_unflipped_host_counts_from_the_bottom() {
        // A pane 300 points tall whose top is 100 points below the top of an 800-point window.
        assert_eq!(
            host_frame(frame(40.0, 100.0, 500.0, 300.0), 800.0, false),
            frame(40.0, 400.0, 500.0, 300.0)
        );
        // Filling the window, and touching its top or bottom edge.
        assert_eq!(
            host_frame(frame(0.0, 0.0, 1200.0, 800.0), 800.0, false),
            frame(0.0, 0.0, 1200.0, 800.0)
        );
        assert_eq!(
            host_frame(frame(0.0, 0.0, 10.0, 20.0), 800.0, false).y,
            780.0
        );
        assert_eq!(
            host_frame(frame(0.0, 780.0, 10.0, 20.0), 800.0, false).y,
            0.0
        );
    }

    #[test]
    fn a_flipped_host_keeps_the_top_left_origin() {
        assert_eq!(
            host_frame(frame(40.0, 100.0, 500.0, 300.0), 800.0, true),
            frame(40.0, 100.0, 500.0, 300.0)
        );
    }

    #[test]
    fn negative_sizes_are_empty() {
        let unflipped = host_frame(frame(5.0, 10.0, -3.0, -4.0), 100.0, false);
        assert_eq!(unflipped, frame(5.0, 90.0, 0.0, 0.0));
        assert_eq!(
            host_frame(frame(5.0, 10.0, -3.0, -4.0), 100.0, true),
            frame(5.0, 10.0, 0.0, 0.0)
        );
    }
}
