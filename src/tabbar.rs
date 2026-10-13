//! The bar along the window's top, in the titlebar beside the traffic lights, where the window is
//! dragged from. With one tab it shows the tab's title, centered; with more, a pill for each tab,
//! equal widths filling the row, and a round + after the last. Pixels in, rectangles out, and
//! what a point is over: no GPUI here, so the geometry and its hit testing are unit tested.

/// The bar's height, with one tab or many, so opening a second tab resizes no pane.
pub const HEIGHT: f32 = 36.;
/// Where the traffic lights sit, from the window's top left: centered in the bar.
pub const TRAFFIC_LIGHTS: (f32, f32) = (14., 11.);
/// Where the row of tabs starts, clear of the traffic lights; a lone tab's title keeps this far
/// from either side.
pub const LIGHTS_END: f32 = 84.;
/// A pill's height; the + button is a circle as tall.
const PILL: f32 = 26.;
/// Between pills, and before the + button.
const GAP: f32 = 6.;
/// The row's margin at the right, and at the left in full screen, where there are no lights.
const EDGE: f32 = 8.;
/// The close button's circle, near a pill's left end, as macOS puts it.
const CLOSE: f32 = 16.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        (self.x..self.x + self.w).contains(&x) && (self.y..self.y + self.h).contains(&y)
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2., self.y + self.h / 2.)
    }
}

/// What a point in the bar is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Tab(usize),
    Close(usize),
    New,
}

/// The bar of a window `width` wide with `tabs` tabs.
#[derive(Clone, Debug, PartialEq)]
pub struct Bar {
    pub height: f32,
    /// Each tab's pill, left to right; none with one tab, whose title fills the bar.
    pub tabs: Vec<Rect>,
    /// The + button, with the pills.
    pub new: Option<Rect>,
}

impl Bar {
    /// In full screen macOS hides the titlebar and its lights: a lone tab needs no bar there, and
    /// the row of many starts at the window's edge.
    pub fn new(width: f32, tabs: usize, fullscreen: bool) -> Bar {
        if tabs < 2 {
            let height = if fullscreen { 0. } else { HEIGHT };
            return Bar { height, tabs: Vec::new(), new: None };
        }
        let top = (HEIGHT - PILL) / 2.;
        let left = if fullscreen { EDGE } else { LIGHTS_END };
        let new = Rect { x: (width - EDGE - PILL).max(left), y: top, w: PILL, h: PILL };
        let row = (new.x - GAP - left).max(0.);
        let count = tabs as f32;
        // Whole points, so the pills' edges and titles stay sharp.
        let w = ((row - GAP * (count - 1.)) / count).floor().max(0.);
        let pill = |index: usize| Rect { x: left + (w + GAP) * index as f32, y: top, w, h: PILL };
        Bar { height: HEIGHT, tabs: (0..tabs).map(pill).collect(), new: Some(new) }
    }

    /// Tab `index`'s close button, at its pill's left end; it shows while the pointer is over
    /// the pill.
    pub fn close(&self, index: usize) -> Rect {
        let pill = self.tabs[index];
        let inset = (PILL - CLOSE) / 2.;
        Rect { x: pill.x + inset, y: pill.y + inset, w: CLOSE, h: CLOSE }
    }

    /// What (`x`, `y`), from the window's top left, is over; `None` for the bar's bare
    /// background, which drags the window, and anywhere below the bar.
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit> {
        if !(0. ..self.height).contains(&y) {
            return None;
        }
        if self.new.is_some_and(|new| new.contains(x, y)) {
            return Some(Hit::New);
        }
        let index = self.tabs.iter().position(|pill| pill.contains(x, y))?;
        Some(if self.close(index).contains(x, y) { Hit::Close(index) } else { Hit::Tab(index) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_tab_is_a_title_bar_with_no_pills() {
        assert_eq!(Bar::new(900., 1, false), Bar { height: HEIGHT, tabs: vec![], new: None });
        assert_eq!(Bar::new(900., 1, true).height, 0.);
        assert_eq!(Bar::new(900., 1, false).hit(450., 18.), None);
    }

    #[test]
    fn pills_share_the_row_after_the_lights_equally_and_the_plus_ends_it() {
        let bar = Bar::new(904., 3, false);
        assert_eq!(bar.height, HEIGHT);
        let new = bar.new.unwrap();
        assert_eq!(new, Rect { x: 870., y: 5., w: 26., h: 26. });
        // 84 to 864 holds three pills and two gaps: 256 each.
        let xs: Vec<(f32, f32)> = bar.tabs.iter().map(|pill| (pill.x, pill.w)).collect();
        assert_eq!(xs, [(84., 256.), (346., 256.), (608., 256.)]);
        assert_eq!(bar.tabs[2].x + bar.tabs[2].w + GAP, new.x);
        assert!(bar.tabs.iter().all(|pill| pill.y == 5. && pill.h == PILL));
        // A share that is no whole number of points rounds down, every pill alike.
        let odd = Bar::new(900., 3, false);
        let xs: Vec<(f32, f32)> = odd.tabs.iter().map(|pill| (pill.x, pill.w)).collect();
        assert_eq!(xs, [(84., 254.), (344., 254.), (604., 254.)]);
        // In full screen the row starts at the edge.
        assert_eq!(Bar::new(900., 3, true).tabs[0].x, EDGE);
        // A window too narrow for them squeezes the pills to nothing, never below.
        let narrow = Bar::new(100., 4, false);
        assert!(narrow.tabs.iter().all(|pill| pill.w == 0.));
        assert_eq!(narrow.new.unwrap().x, LIGHTS_END);
    }

    #[test]
    fn a_point_is_over_a_pill_its_close_button_the_plus_or_nothing() {
        let bar = Bar::new(900., 2, false);
        let (first, second) = (bar.tabs[0], bar.tabs[1]);
        assert_eq!(bar.hit(first.x + first.w / 2., 18.), Some(Hit::Tab(0)));
        assert_eq!(bar.hit(second.x + second.w - 1., 30.), Some(Hit::Tab(1)));
        let (x, y) = bar.close(1).center();
        assert_eq!((x, y), (second.x + 13., 18.));
        assert_eq!(bar.hit(x, y), Some(Hit::Close(1)));
        let (x, y) = bar.new.unwrap().center();
        assert_eq!(bar.hit(x, y), Some(Hit::New));
        // The gap between pills, the lights' corner, the bar's rim and below it are bare.
        assert_eq!(bar.hit(first.x + first.w + GAP / 2., 18.), None);
        assert_eq!(bar.hit(40., 18.), None);
        assert_eq!(bar.hit(first.x + 10., 2.), None);
        assert_eq!(bar.hit(first.x + 10., HEIGHT + 1.), None);
    }
}
