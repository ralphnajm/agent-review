//! Drawing over a screenshot of the product: marker strokes, arrows, boxes,
//! sticky notes and text. The user's comment sends the result to the agent as
//! one image.
//!
//! Everything is kept in the screenshot's own pixels, and drawn scaled to fit
//! the widget, so the exported image matches what the user saw.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use gtk::{cairo, gdk, glib, pango};

/// What a drag on the whiteboard draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Marker,
    Arrow,
    Box,
    /// A yellow note with text, placed with a click.
    Sticky,
    /// Text on its own, placed with a click.
    Text,
}

type Rgb = (f64, f64, f64);

/// The colours of the marker, arrows, boxes and text, by name.
pub const COLORS: [(&str, Rgb); 4] = [
    ("Red", (0.88, 0.11, 0.14)),
    ("Blue", (0.11, 0.44, 0.85)),
    ("Green", (0.15, 0.6, 0.33)),
    ("Black", (0.1, 0.1, 0.1)),
];

const STICKY: Rgb = (1.0, 0.91, 0.45);

#[derive(Debug, Clone)]
enum Item {
    Stroke { color: Rgb, points: Vec<(f64, f64)> },
    Arrow { color: Rgb, from: (f64, f64), to: (f64, f64) },
    Box { color: Rgb, from: (f64, f64), to: (f64, f64) },
    Sticky { at: (f64, f64), text: String },
    Text { color: Rgb, at: (f64, f64), text: String },
}

#[derive(Clone)]
pub struct Whiteboard(Rc<Inner>);

struct Inner {
    area: gtk::DrawingArea,
    image: RefCell<Option<cairo::ImageSurface>>,
    items: RefCell<Vec<Item>>,
    /// The item a drag is drawing.
    drawing: RefCell<Option<Item>>,
    /// Where the drag started, in the widget.
    drag_start: Cell<(f64, f64)>,
    tool: Cell<Tool>,
    color: Cell<Rgb>,
    /// Asks for the text of a sticky note or text, where it goes.
    popover: gtk::Popover,
    entry: gtk::Entry,
    /// Where the text being typed goes, in the screenshot.
    pending: Cell<Option<(f64, f64)>>,
    on_change: RefCell<Option<Box<dyn Fn()>>>,
}

impl Whiteboard {
    pub fn new() -> Self {
        let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).build();
        area.add_css_class("whiteboard");
        area.set_cursor_from_name(Some("crosshair"));
        let entry = gtk::Entry::builder().width_chars(28).build();
        let popover = gtk::Popover::builder().child(&entry).autohide(true).build();
        popover.set_parent(&area);

        let this = Self(Rc::new(Inner {
            area,
            image: RefCell::default(),
            items: RefCell::default(),
            drawing: RefCell::default(),
            drag_start: Cell::new((0.0, 0.0)),
            tool: Cell::new(Tool::Marker),
            color: Cell::new(COLORS[0].1),
            popover,
            entry,
            pending: Cell::new(None),
            on_change: RefCell::default(),
        }));

        let weak = this.weak();
        this.0.area.set_draw_func(move |_, cr, width, height| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).draw(cr, f64::from(width), f64::from(height));
            }
        });

        let drag = gtk::GestureDrag::new();
        let weak = this.weak();
        drag.connect_drag_begin(move |_, x, y| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).begin(x, y);
            }
        });
        let weak = this.weak();
        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).update(dx, dy);
            }
        });
        let weak = this.weak();
        drag.connect_drag_end(move |_, dx, dy| {
            if let Some(inner) = weak.upgrade() {
                Self(inner).end(dx, dy);
            }
        });
        this.0.area.add_controller(drag);

        let weak = this.weak();
        this.0.entry.connect_activate(move |entry| {
            let Some(inner) = weak.upgrade() else { return };
            let text = entry.text().trim().to_owned();
            inner.popover.popdown();
            let Some(at) = inner.pending.take().filter(|_| !text.is_empty()) else { return };
            let item = match inner.tool.get() {
                Tool::Sticky => Item::Sticky { at, text },
                _ => Item::Text { color: inner.color.get(), at, text },
            };
            Self(inner).push(item);
        });
        this
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.0.area
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    /// Called whenever the drawing changes.
    pub fn connect_change(&self, callback: impl Fn() + 'static) {
        self.0.on_change.replace(Some(Box::new(callback)));
    }

    pub fn set_tool(&self, tool: Tool) {
        self.0.tool.set(tool);
    }

    pub fn set_color(&self, color: Rgb) {
        self.0.color.set(color);
    }

    /// Starts over on `screenshot`.
    pub fn set_image(&self, screenshot: &gdk::Texture) -> Result<(), cairo::Error> {
        let (width, height) = (screenshot.width(), screenshot.height());
        let stride = cairo::Format::ARgb32.stride_for_width(u32::try_from(width).unwrap_or(0))?;
        let mut data = vec![0; usize::try_from(stride * height).unwrap_or(0)];
        // Textures download in cairo's ARGB32 layout.
        screenshot.download(&mut data, usize::try_from(stride).unwrap_or(0));
        let surface = cairo::ImageSurface::create_for_data(data, cairo::Format::ARgb32, width, height, stride)?;
        self.0.image.replace(Some(surface));
        self.0.items.borrow_mut().clear();
        self.0.drawing.replace(None);
        self.changed();
        Ok(())
    }

    /// Whether the user drew anything.
    pub fn has_drawing(&self) -> bool {
        !self.0.items.borrow().is_empty()
    }

    /// The text of the sticky notes and text on the whiteboard, in order.
    pub fn texts(&self) -> Vec<String> {
        self.0
            .items
            .borrow()
            .iter()
            .filter_map(|item| match item {
                Item::Sticky { text, .. } | Item::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn undo(&self) {
        self.0.items.borrow_mut().pop();
        self.changed();
    }

    /// Saves the screenshot with the drawing on it as a PNG, scaled down to at
    /// most `max_width` pixels wide.
    pub fn export(&self, path: &Path, max_width: i32) -> Result<(), String> {
        let image = self.0.image.borrow();
        let image = image.as_ref().ok_or("There is no screenshot")?;
        let (width, height) = (image.width(), image.height());
        let scale = (f64::from(max_width) / f64::from(width)).min(1.0);
        #[allow(clippy::cast_possible_truncation)]
        let (out_width, out_height) =
            ((f64::from(width) * scale).round() as i32, (f64::from(height) * scale).round() as i32);
        let mut out =
            cairo::ImageSurface::create(cairo::Format::ARgb32, out_width, out_height).map_err(|e| e.to_string())?;
        {
            let cr = cairo::Context::new(&out).map_err(|e| e.to_string())?;
            cr.scale(scale, scale);
            self.paint(&cr, image);
        }
        out.flush();
        let stride = usize::try_from(out.stride()).unwrap_or(0);
        let data = out.data().map_err(|e| e.to_string())?;
        let texture = gdk::MemoryTexture::new(
            out_width,
            out_height,
            gdk::MemoryFormat::B8g8r8a8Premultiplied,
            &glib::Bytes::from(&data[..]),
            stride,
        );
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        texture.save_to_png(path).map_err(|e| e.to_string())
    }

    fn changed(&self) {
        self.0.area.queue_draw();
        if let Some(callback) = self.0.on_change.borrow().as_ref() {
            callback();
        }
    }

    fn push(&self, item: Item) {
        self.0.items.borrow_mut().push(item);
        self.changed();
    }

    /// How the screenshot fits the widget: its scale, then its offset.
    fn fit(&self) -> Option<(f64, (f64, f64))> {
        let image = self.0.image.borrow();
        let image = image.as_ref()?;
        let (width, height) = (f64::from(self.0.area.width()), f64::from(self.0.area.height()));
        let (image_width, image_height) = (f64::from(image.width()), f64::from(image.height()));
        let scale = (width / image_width).min(height / image_height);
        Some((scale, ((width - image_width * scale) / 2.0, (height - image_height * scale) / 2.0)))
    }

    /// A point of the widget, in the screenshot's pixels.
    fn to_image(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (scale, (left, top)) = self.fit()?;
        Some(((x - left) / scale, (y - top) / scale))
    }

    fn begin(&self, x: f64, y: f64) {
        self.0.drag_start.set((x, y));
        let Some(at) = self.to_image(x, y) else { return };
        let color = self.0.color.get();
        let item = match self.0.tool.get() {
            Tool::Marker => Item::Stroke { color, points: vec![at] },
            Tool::Arrow => Item::Arrow { color, from: at, to: at },
            Tool::Box => Item::Box { color, from: at, to: at },
            Tool::Sticky | Tool::Text => return,
        };
        self.0.drawing.replace(Some(item));
    }

    fn update(&self, dx: f64, dy: f64) {
        let (x, y) = self.0.drag_start.get();
        let Some(at) = self.to_image(x + dx, y + dy) else { return };
        if let Some(item) = self.0.drawing.borrow_mut().as_mut() {
            match item {
                Item::Stroke { points, .. } => points.push(at),
                Item::Arrow { to, .. } | Item::Box { to, .. } => *to = at,
                Item::Sticky { .. } | Item::Text { .. } => {}
            }
        }
        self.0.area.queue_draw();
    }

    fn end(&self, dx: f64, dy: f64) {
        let (x, y) = self.0.drag_start.get();
        match self.0.drawing.take() {
            // A click with the arrow or box tools draws nothing.
            Some(item @ (Item::Arrow { .. } | Item::Box { .. })) if dx.hypot(dy) < 4.0 => drop(item),
            Some(item) => self.push(item),
            None => {
                let Some(at) = self.to_image(x + dx, y + dy) else { return };
                self.0.pending.set(Some(at));
                let entry = &self.0.entry;
                entry.set_text("");
                entry.set_placeholder_text(Some(if self.0.tool.get() == Tool::Sticky {
                    "Sticky note, then Enter"
                } else {
                    "Text, then Enter"
                }));
                #[allow(clippy::cast_possible_truncation)]
                let point = gdk::Rectangle::new((x + dx) as i32, (y + dy) as i32, 1, 1);
                self.0.popover.set_pointing_to(Some(&point));
                self.0.popover.popup();
                entry.grab_focus();
            }
        }
    }

    fn draw(&self, cr: &cairo::Context, width: f64, height: f64) {
        cr.set_source_rgb(0.5, 0.5, 0.5);
        cr.rectangle(0.0, 0.0, width, height);
        let _ = cr.fill();
        let (Some((scale, (left, top))), Some(image)) = (self.fit(), self.0.image.borrow().clone()) else { return };
        cr.translate(left, top);
        cr.scale(scale, scale);
        self.paint(cr, &image);
        if let Some(item) = self.0.drawing.borrow().as_ref() {
            draw_item(cr, item, unit(&image));
        }
    }

    /// Paints the screenshot and the drawing in the screenshot's pixels.
    fn paint(&self, cr: &cairo::Context, image: &cairo::ImageSurface) {
        let _ = cr.set_source_surface(image, 0.0, 0.0);
        let _ = cr.paint();
        let unit = unit(image);
        for item in self.0.items.borrow().iter() {
            draw_item(cr, item, unit);
        }
    }
}

/// The size of a pixel on a regular screen, in the screenshot's pixels, so
/// lines and text look the same on high-resolution screenshots.
fn unit(image: &cairo::ImageSurface) -> f64 {
    (f64::from(image.width()) / 1400.0).max(1.0)
}

fn draw_item(cr: &cairo::Context, item: &Item, unit: f64) {
    cr.save().ok();
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    cr.set_line_width(4.0 * unit);
    match item {
        Item::Stroke { color, points } => {
            set_color(cr, *color);
            if let Some(&(x, y)) = points.first() {
                cr.move_to(x, y);
                // A single point still shows, as a dot.
                cr.line_to(x, y);
            }
            for &(x, y) in points.iter().skip(1) {
                cr.line_to(x, y);
            }
            let _ = cr.stroke();
        }
        Item::Arrow { color, from, to } => {
            set_color(cr, *color);
            cr.move_to(from.0, from.1);
            cr.line_to(to.0, to.1);
            let angle = (to.1 - from.1).atan2(to.0 - from.0);
            for side in [-1.0, 1.0] {
                let wing = angle + std::f64::consts::PI - side * 0.45;
                cr.move_to(to.0, to.1);
                cr.line_to(to.0 + 20.0 * unit * wing.cos(), to.1 + 20.0 * unit * wing.sin());
            }
            let _ = cr.stroke();
        }
        Item::Box { color, from, to } => {
            set_color(cr, *color);
            cr.rectangle(from.0.min(to.0), from.1.min(to.1), (to.0 - from.0).abs(), (to.1 - from.1).abs());
            let _ = cr.stroke();
        }
        Item::Sticky { at, text } => {
            let layout = text_layout(cr, text, 16.0 * unit, Some(220.0 * unit));
            let (width, height) = layout.pixel_size();
            let pad = 12.0 * unit;
            let (w, h) = (f64::from(width) + 2.0 * pad, f64::from(height) + 2.0 * pad);
            cr.set_source_rgba(0.0, 0.0, 0.0, 0.2);
            cr.rectangle(at.0 + 3.0 * unit, at.1 + 4.0 * unit, w, h);
            let _ = cr.fill();
            set_color(cr, STICKY);
            cr.rectangle(at.0, at.1, w, h);
            let _ = cr.fill();
            cr.set_source_rgb(0.15, 0.13, 0.05);
            cr.move_to(at.0 + pad, at.1 + pad);
            pangocairo::functions::show_layout(cr, &layout);
        }
        Item::Text { color, at, text } => {
            let layout = text_layout(cr, text, 22.0 * unit, None);
            cr.move_to(at.0, at.1);
            // A white outline keeps the text readable on any background.
            pangocairo::functions::layout_path(cr, &layout);
            cr.set_source_rgb(1.0, 1.0, 1.0);
            cr.set_line_width(5.0 * unit);
            let _ = cr.stroke_preserve();
            set_color(cr, *color);
            let _ = cr.fill();
        }
    }
    cr.restore().ok();
}

fn set_color(cr: &cairo::Context, (r, g, b): Rgb) {
    cr.set_source_rgb(r, g, b);
}

fn text_layout(cr: &cairo::Context, text: &str, size: f64, wrap: Option<f64>) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = pango::FontDescription::from_string("Sans Bold");
    #[allow(clippy::cast_possible_truncation)]
    font.set_absolute_size(size * f64::from(pango::SCALE));
    layout.set_font_description(Some(&font));
    if let Some(width) = wrap {
        #[allow(clippy::cast_possible_truncation)]
        layout.set_width((width * f64::from(pango::SCALE)) as i32);
        layout.set_wrap(pango::WrapMode::WordChar);
    }
    layout.set_text(text);
    layout
}
