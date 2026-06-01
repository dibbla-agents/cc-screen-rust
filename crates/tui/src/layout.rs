//! The six fixed grid layouts (ported from the web app's `TileGrid`) and the
//! geometry that turns a layout + area into per-box rectangles. Box order is
//! pane-index order, with slot 0 the primary box — matching the web's
//! `pane(i)` mapping so a session migrated to slot 0 lands in the big box.

use ratatui::layout::{Constraint, Layout as RLayout, Rect};
use ratatui::widgets::{Block, Borders};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    Single,
    Stacked,    // two rows
    SideBySide, // two columns
    LeftL,      // left column tall, right column split
    RightL,     // right column tall, left column split
    Quad,       // 2×2
}

impl Layout {
    /// Display / digit order, matching the web palette (digit 1..=6).
    pub const ALL: [Layout; 6] = [
        Layout::Single,
        Layout::Stacked,
        Layout::SideBySide,
        Layout::LeftL,
        Layout::RightL,
        Layout::Quad,
    ];

    /// Layout for a 1-based digit shortcut (`1`..=`6`).
    pub fn from_digit(d: u8) -> Option<Layout> {
        Layout::ALL.get((d as usize).checked_sub(1)?).copied()
    }

    /// Number of boxes in this layout.
    pub fn count(self) -> usize {
        match self {
            Layout::Single => 1,
            Layout::Stacked | Layout::SideBySide => 2,
            Layout::LeftL | Layout::RightL => 3,
            Layout::Quad => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Layout::Single => "single",
            Layout::Stacked => "stacked",
            Layout::SideBySide => "side-by-side",
            Layout::LeftL => "left-L",
            Layout::RightL => "right-L",
            Layout::Quad => "quad",
        }
    }
}

fn split_h(area: Rect) -> [Rect; 2] {
    let r = RLayout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    [r[0], r[1]]
}

fn split_v(area: Rect) -> [Rect; 2] {
    let r = RLayout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    [r[0], r[1]]
}

/// Outer rectangle per box, in pane-index order.
pub fn tiles(layout: Layout, area: Rect) -> Vec<Rect> {
    match layout {
        Layout::Single => vec![area],
        Layout::SideBySide => {
            let [a, b] = split_h(area);
            vec![a, b]
        }
        Layout::Stacked => {
            let [a, b] = split_v(area);
            vec![a, b]
        }
        Layout::Quad => {
            let [top, bot] = split_v(area);
            let [tl, tr] = split_h(top);
            let [bl, br] = split_h(bot);
            vec![tl, tr, bl, br]
        }
        Layout::LeftL => {
            let [left, right] = split_h(area);
            let [rt, rb] = split_v(right);
            vec![left, rt, rb] // slot 0 = left tall
        }
        Layout::RightL => {
            let [left, right] = split_h(area);
            let [lt, lb] = split_v(left);
            vec![right, lt, lb] // slot 0 = right tall
        }
    }
}

/// Whether boxes get a border (everything but `Single`).
pub fn bordered(layout: Layout) -> bool {
    layout.count() > 1
}

/// Inner (drawable) rectangle per box. Sizing (pane resize) and rendering both
/// call this so the emulator size matches what's painted.
pub fn inner_rects(layout: Layout, area: Rect) -> Vec<Rect> {
    let b = bordered(layout);
    tiles(layout, area)
        .into_iter()
        .map(|t| if b { Block::default().borders(Borders::ALL).inner(t) } else { t })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new(0, 0, 100, 40)
    }

    #[test]
    fn counts_match() {
        for l in Layout::ALL {
            assert_eq!(tiles(l, area()).len(), l.count(), "{l:?}");
        }
    }

    #[test]
    fn digit_roundtrip() {
        assert_eq!(Layout::from_digit(1), Some(Layout::Single));
        assert_eq!(Layout::from_digit(6), Some(Layout::Quad));
        assert_eq!(Layout::from_digit(0), None);
        assert_eq!(Layout::from_digit(7), None);
    }

    #[test]
    fn single_is_whole_area() {
        assert_eq!(tiles(Layout::Single, area()), vec![area()]);
    }

    #[test]
    fn side_by_side_splits_width() {
        let t = tiles(Layout::SideBySide, area());
        assert_eq!(t[0].height, 40);
        assert_eq!(t[1].height, 40);
        assert_eq!(t[0].x, 0);
        assert_eq!(t[1].x, 50);
        assert_eq!(t[0].width + t[1].width, 100);
    }

    #[test]
    fn stacked_splits_height() {
        let t = tiles(Layout::Stacked, area());
        assert_eq!(t[0].width, 100);
        assert_eq!(t[1].y, 20);
        assert_eq!(t[0].height + t[1].height, 40);
    }

    #[test]
    fn quad_is_two_by_two() {
        let t = tiles(Layout::Quad, area());
        assert_eq!(t.len(), 4);
        // tl, tr, bl, br
        assert_eq!((t[0].x, t[0].y), (0, 0));
        assert_eq!((t[1].x, t[1].y), (50, 0));
        assert_eq!((t[2].x, t[2].y), (0, 20));
        assert_eq!((t[3].x, t[3].y), (50, 20));
    }

    #[test]
    fn left_l_primary_is_tall_left() {
        let t = tiles(Layout::LeftL, area());
        assert_eq!(t[0].x, 0);
        assert_eq!(t[0].height, 40); // left column spans full height
        assert_eq!(t[1].x, 50); // right top
        assert_eq!(t[2].x, 50); // right bottom
        assert!(t[2].y > t[1].y);
    }

    #[test]
    fn right_l_primary_is_tall_right() {
        let t = tiles(Layout::RightL, area());
        assert_eq!(t[0].x, 50);
        assert_eq!(t[0].height, 40); // right column spans full height
        assert_eq!(t[1].x, 0); // left top
        assert_eq!(t[2].x, 0); // left bottom
    }

    #[test]
    fn inner_shrinks_for_bordered_layouts() {
        let outer = tiles(Layout::Quad, area());
        let inner = inner_rects(Layout::Quad, area());
        for (o, i) in outer.iter().zip(&inner) {
            assert_eq!(i.width, o.width - 2); // minus L/R border
            assert_eq!(i.height, o.height - 2);
        }
        // single is borderless → inner == outer
        assert_eq!(inner_rects(Layout::Single, area()), tiles(Layout::Single, area()));
    }
}
