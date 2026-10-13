//! The scrolled window every file's diff shares, with helpers for the part
//! of a widget inside it that is on screen.

use std::cell::Cell;
use std::rc::Rc;

use gtk::prelude::*;

#[derive(Clone)]
pub struct Scroller(Rc<Inner>);

struct Inner {
    window: gtk::ScrolledWindow,
    content: gtk::Box,
    /// Height of the file header stuck to the top, which covers what is under it.
    inset: Cell<f64>,
}

impl Scroller {
    pub fn new() -> Self {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // Not scrolled to the focus: a diff taking it would jump to its top or bottom, and the
        // composer takes it before it is laid out where it opens. `reveal` scrolls instead.
        let viewport = gtk::Viewport::builder().child(&content).scroll_to_focus(false).build();
        // Never sideways: long lines wrap.
        let window = gtk::ScrolledWindow::builder()
            .child(&viewport)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .hexpand(true)
            .build();
        Self(Rc::new(Inner { window, content, inset: Cell::new(0.0) }))
    }

    pub fn window(&self) -> &gtk::ScrolledWindow {
        &self.0.window
    }

    /// The box holding the file sections.
    pub fn content(&self) -> &gtk::Box {
        &self.0.content
    }

    pub fn inset(&self) -> f64 {
        self.0.inset.get()
    }

    pub fn set_inset(&self, inset: f64) {
        self.0.inset.set(inset);
    }

    /// The top of `widget` in content coordinates.
    pub fn offset_of(&self, widget: &impl IsA<gtk::Widget>) -> Option<f64> {
        widget.compute_point(&self.0.content, &gtk::graphene::Point::zero()).map(|p| f64::from(p.y()))
    }

    /// The heights of `widget`, in its own coordinates, that are on screen and
    /// not under the sticky header: `(top, bottom)`.
    pub fn visible_span(&self, widget: &impl IsA<gtk::Widget>) -> Option<(f64, f64)> {
        let origin = widget.compute_point(&self.0.window, &gtk::graphene::Point::zero())?;
        let y = f64::from(origin.y());
        Some((self.0.inset.get() - y, f64::from(self.0.window.height()) - y))
    }

    /// Scrolls the least needed to show heights `top..bottom` of `widget`,
    /// favouring `top` when they don't fit.
    pub fn reveal(&self, widget: &impl IsA<gtk::Widget>, top: f64, bottom: f64) {
        let Some((visible_top, visible_bottom)) = self.visible_span(widget) else { return };
        let delta = if top < visible_top {
            top - visible_top
        } else if bottom > visible_bottom {
            (bottom - visible_bottom).min(top - visible_top)
        } else {
            return;
        };
        self.scroll_by(delta);
    }

    pub fn scroll_by(&self, delta: f64) {
        let adjustment = self.0.window.vadjustment();
        adjustment.set_value(adjustment.value() + delta);
    }
}
