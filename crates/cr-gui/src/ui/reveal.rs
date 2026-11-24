//! Keeps the keyboard focus in view.
//!
//! GPUI moves focus without scrolling, so Tab can land on a control that is
//! scrolled out of sight. When the focus moves by keyboard the app dispatches
//! [`RevealFocus`] from the focused element. It bubbles through that
//! element's ancestors: the innermost [`RevealOnFocus::reveal_on_focus`] block
//! reports where it is, and each [`FollowFocus::follow_focus`] scroll area on
//! the way out scrolls just far enough to show it.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui_kit::component::*;
use gpui_kit::*;

actions!(reveal, [RevealFocus]);

/// Space kept between a revealed block and the edge of its scroll area.
const MARGIN: f32 = 12.;

/// Where the block holding the focus is, while [`RevealFocus`] bubbles.
#[derive(Default)]
struct Target(Option<Bounds<Pixels>>);

impl Global for Target {}

/// A block that is scrolled into view when the keyboard focuses a control
/// inside it. Mark the row or card around a control, not a large section.
pub trait RevealOnFocus: InteractiveElement + ParentElement + Sized {
    fn reveal_on_focus(self) -> Self {
        let bounds = Rc::new(Cell::new(None));
        let seen = bounds.clone();
        self.on_prepaint(move |bounds, _, _| seen.set(Some(bounds)))
            .on_action(move |_: &RevealFocus, _, cx| {
                cx.propagate();
                let target = cx.default_global::<Target>();
                // Blocks hear this innermost first, and the innermost wins.
                if target.0.is_none() {
                    target.0 = bounds.get();
                }
            })
    }
}

impl RevealOnFocus for Div {}
impl RevealOnFocus for Stateful<Div> {}

/// Wraps a control that has no row or card of its own to mark.
pub fn revealed(control: impl IntoElement) -> Div {
    v_flex().child(control).reveal_on_focus()
}

/// Marks each control of a wrapping group on its own. A group of any length
/// can grow taller than its scroll area, and a block that tall is only shown
/// from its top.
pub fn revealed_each<E: IntoElement>(
    controls: impl IntoIterator<Item = E>,
) -> impl Iterator<Item = Div> {
    controls.into_iter().map(revealed)
}

/// A scroll area that scrolls to the block holding the keyboard focus.
pub trait FollowFocus: StatefulInteractiveElement + Sized {
    fn follow_focus(self, handle: &ScrollHandle) -> Self {
        let scroll = handle.clone();
        self.track_scroll(handle)
            .on_action(move |_: &RevealFocus, window, cx| {
                cx.propagate();
                let Some(target) = cx.default_global::<Target>().0 else {
                    return;
                };
                let (view, from, max) = (scroll.bounds(), scroll.offset(), scroll.max_offset());
                let to = point(
                    px(offset_showing(
                        from.x.into(),
                        max.x.into(),
                        (target.left().into(), target.right().into()),
                        (view.left().into(), view.right().into()),
                    )),
                    px(offset_showing(
                        from.y.into(),
                        max.y.into(),
                        (target.top().into(), target.bottom().into()),
                        (view.top().into(), view.bottom().into()),
                    )),
                );
                if to != from {
                    scroll.set_offset(to);
                    // An outer scroll area sees the block where it is now.
                    cx.default_global::<Target>().0 = Some(target + (to - from));
                    window.refresh();
                }
            })
    }
}

impl FollowFocus for Stateful<Div> {}

/// The scroll offset that brings `start..end` inside the view, moving as
/// little as possible. Offsets run from 0 down to `-max`. Something longer
/// than the view is shown from its start.
fn offset_showing(offset: f32, max: f32, (start, end): (f32, f32), view: (f32, f32)) -> f32 {
    let margin = MARGIN.min(((view.1 - view.0) - (end - start)).max(0.) / 2.);
    let (first, last) = (view.0 + margin, view.1 - margin);
    let shift = if start < first {
        first - start
    } else if end > last {
        (last - end).max(first - start)
    } else {
        0.
    };
    (offset + shift).clamp(-max.max(0.), 0.)
}

/// Per-window state for revealing the focus: the last focused element, and
/// the scroll handles of the areas that have none of their own.
#[derive(Default)]
pub struct FocusReveal {
    focused: Option<WeakFocusHandle>,
    frame: u64,
    handles: RefCell<HashMap<&'static str, (u64, ScrollHandle)>>,
}

impl FocusReveal {
    /// Call once per render. Asks for the focus to be revealed when it moved
    /// by keyboard. A click focuses something already under the pointer, and
    /// scrolling then would move it away before the button is released.
    pub fn begin_frame(&mut self, window: &mut Window, cx: &mut App) {
        self.frame += 1;
        let focused = window.focused(cx).map(|handle| handle.downgrade());
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        if window.last_input_was_keyboard() {
            cx.default_global::<Target>().0 = None;
            window.dispatch_action(Box::new(RevealFocus), cx);
        }
    }

    /// The scroll handle of the area called `id`. An area starts at the top
    /// again once it was not drawn for a frame, as an untracked one does.
    pub fn scroll(&self, id: &'static str) -> ScrollHandle {
        let mut handles = self.handles.borrow_mut();
        let (used, handle) = handles.entry(id).or_default();
        if *used + 1 < self.frame {
            *handle = ScrollHandle::new();
        }
        *used = self.frame;
        handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: (f32, f32) = (100., 300.);

    #[core::prelude::v1::test]
    fn a_block_in_view_does_not_scroll() {
        assert_eq!(offset_showing(-50., 500., (150., 180.), VIEW), -50.);
    }

    #[core::prelude::v1::test]
    fn a_block_below_the_view_scrolls_up_to_the_bottom_edge() {
        // Its bottom at 340 has to reach 300 less the margin.
        assert_eq!(offset_showing(-50., 500., (310., 340.), VIEW), -102.);
    }

    #[core::prelude::v1::test]
    fn a_block_above_the_view_scrolls_down_to_the_top_edge() {
        assert_eq!(offset_showing(-50., 500., (60., 90.), VIEW), 0.);
        assert_eq!(offset_showing(-200., 500., (60., 90.), VIEW), -148.);
    }

    #[core::prelude::v1::test]
    fn a_block_taller_than_the_view_is_shown_from_its_top() {
        assert_eq!(offset_showing(-50., 500., (250., 600.), VIEW), -200.);
    }

    #[core::prelude::v1::test]
    fn the_offset_stays_inside_the_scroll_range() {
        assert_eq!(offset_showing(-90., 100., (310., 340.), VIEW), -100.);
        assert_eq!(offset_showing(0., 0., (310., 340.), VIEW), 0.);
    }

    #[core::prelude::v1::test]
    fn an_area_that_was_closed_starts_at_the_top_again() {
        let mut reveal = FocusReveal {
            frame: 1,
            ..Default::default()
        };
        let first = reveal.scroll("area");
        first.set_offset(point(px(0.), px(-40.)));
        reveal.frame = 2;
        assert_eq!(reveal.scroll("area").offset().y, px(-40.));
        // Not drawn in frame 3.
        reveal.frame = 4;
        assert_eq!(reveal.scroll("area").offset().y, px(0.));
    }
}
