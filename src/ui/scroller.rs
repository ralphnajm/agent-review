//! The scrolled window every file's diff shares, with helpers for the part
//! of a widget inside it that is on screen, and `Slice`, which lays out and
//! draws only that part of a diff.

use std::cell::Cell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

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

glib::wrapper! {
    /// Holds a scrollable widget, such as a text view, as tall as its content,
    /// but allocates it only the part of that height on screen in the `Scroller`,
    /// scrolled to match. A text view inside lays out and draws only the lines on
    /// screen, rather than every line of a long diff each time it changes.
    pub struct Slice(ObjectSubclass<imp::Slice>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Slice {
    pub fn new(child: &(impl IsA<gtk::Widget> + IsA<gtk::Scrollable>), scroller: &Scroller) -> Self {
        let this: Self = glib::Object::new();
        child.set_parent(&this);
        let imp = this.imp();
        imp.child.replace(Some(child.clone().upcast()));
        imp.scroller.replace(Some(scroller.clone()));

        let mut handlers = Vec::new();
        // The part on screen moves as the panel scrolls or resizes.
        let outer = scroller.window().vadjustment();
        let weak = this.downgrade();
        handlers.push((
            outer.clone(),
            outer.connect_value_changed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.update();
                }
            }),
        ));
        let weak = this.downgrade();
        handlers.push((
            outer.clone(),
            outer.connect_changed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.update();
                }
            }),
        ));
        // The child scrolling itself, as a text view does to keep its text in place when the
        // lines above change height, or to follow a selection: scroll the panel instead.
        if let Some(inner) = child.vadjustment() {
            let weak = this.downgrade();
            handlers.push((
                inner.clone(),
                inner.connect_value_changed(move |adjustment| {
                    let Some(this) = weak.upgrade() else { return };
                    let imp = this.imp();
                    // Hidden diffs, such as collapsed ones, don't move what is shown.
                    if imp.allocating.get() || !this.is_mapped() {
                        return;
                    }
                    let delta = adjustment.value() - f64::from(imp.span.get().0);
                    if delta.abs() >= 1.0
                        && let Some(scroller) = imp.scroller.borrow().as_ref()
                    {
                        scroller.scroll_by(delta);
                    }
                    this.queue_allocate();
                }),
            ));
        }
        imp.handlers.replace(handlers);
        this
    }

    /// The heights of this widget on screen, `(top, bottom)`, within `0..height`.
    fn on_screen(&self, height: i32) -> (i32, i32) {
        let Some(scroller) = self.imp().scroller.borrow().clone() else { return (0, height) };
        let window = scroller.window();
        let Some(origin) = window.compute_point(self, &gtk::graphene::Point::zero()) else { return (0, 0) };
        let y = f64::from(origin.y());
        #[allow(clippy::cast_possible_truncation)]
        let (top, bottom) = (y.floor() as i32, (y + f64::from(window.height())).ceil() as i32);
        let top = top.clamp(0, height);
        (top, bottom.clamp(top, height))
    }

    /// Reallocates the child for the part now on screen. Not compared with the
    /// current part: the panel has yet to move this widget to where it scrolled.
    fn update(&self) {
        if self.is_mapped() {
            self.queue_allocate();
        }
    }
}

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{glib, graphene, gsk};

    use super::Scroller;

    #[derive(Default)]
    pub struct Slice {
        pub child: RefCell<Option<gtk::Widget>>,
        pub scroller: RefCell<Option<Scroller>>,
        /// The heights the child is allocated, `(top, bottom)`.
        pub span: Cell<(i32, i32)>,
        /// Set while the child is allocated, which configures its adjustment.
        pub allocating: Cell<bool>,
        pub handlers: RefCell<Vec<(gtk::Adjustment, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Slice {
        const NAME: &'static str = "AgentReviewSlice";
        type Type = super::Slice;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Slice {
        fn dispose(&self) {
            for (adjustment, handler) in self.handlers.take() {
                adjustment.disconnect(handler);
            }
            if let Some(child) = self.child.take() {
                child.unparent();
            }
            self.scroller.take();
        }
    }

    impl WidgetImpl for Slice {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.child.borrow().as_ref().map_or(gtk::SizeRequestMode::ConstantSize, WidgetExt::request_mode)
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.child.borrow().as_ref().map_or((0, 0, -1, -1), |child| child.measure(orientation, for_size))
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.child.borrow().clone() else { return };
            let (top, bottom) = self.obj().on_screen(height);
            self.span.set((top, bottom));
            self.allocating.set(true);
            // Scrolled to the part on screen before the child is allocated, which keeps the value.
            if let Some(adjustment) = child.dynamic_cast_ref::<gtk::Scrollable>().and_then(ScrollableExt::vadjustment) {
                let page = f64::from(bottom - top);
                adjustment.configure(f64::from(top), 0.0, f64::from(height), page / 10.0, page * 0.9, page);
            }
            #[allow(clippy::cast_precision_loss)]
            let at = gsk::Transform::new().translate(&graphene::Point::new(0.0, top as f32));
            child.allocate(width, bottom - top, -1, Some(at));
            self.allocating.set(false);
        }
    }
}
