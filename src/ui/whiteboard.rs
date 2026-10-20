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

/// A sticky note's paper, top to bottom, and its curled corner.
const STICKY_TOP: Rgb = (1.0, 0.94, 0.55);
const STICKY_BOTTOM: Rgb = (0.99, 0.87, 0.38);
const STICKY_CURL: Rgb = (0.91, 0.78, 0.3);

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
    /// What Undo took back, last first, for Redo; drawing anything new forgets it.
    undone: RefCell<Vec<Item>>,
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
    on_change: RefCell<Vec<Box<dyn Fn()>>>,
}

impl Whiteboard {
    pub fn new() -> Self {
        // Focusable, for the whiteboard's keys.
        let area = gtk::DrawingArea::builder().hexpand(true).vexpand(true).focusable(true).build();
        area.add_css_class("whiteboard");
        area.set_cursor_from_name(Some("crosshair"));
        let entry = gtk::Entry::builder().width_chars(28).build();
        let popover = gtk::Popover::builder().child(&entry).autohide(true).build();
        popover.set_parent(&area);

        let this = Self(Rc::new(Inner {
            area,
            image: RefCell::default(),
            items: RefCell::default(),
            undone: RefCell::default(),
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
        self.0.on_change.borrow_mut().push(Box::new(callback));
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
        self.0.undone.borrow_mut().clear();
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
        let item = self.0.items.borrow_mut().pop();
        if let Some(item) = item {
            self.0.undone.borrow_mut().push(item);
            self.changed();
        }
    }

    pub fn redo(&self) {
        let item = self.0.undone.borrow_mut().pop();
        if let Some(item) = item {
            self.0.items.borrow_mut().push(item);
            self.changed();
        }
    }

    fn can_redo(&self) -> bool {
        !self.0.undone.borrow().is_empty()
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
        for callback in self.0.on_change.borrow().iter() {
            callback();
        }
    }

    fn push(&self, item: Item) {
        self.0.undone.borrow_mut().clear();
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

/// The whiteboard's tools, colours, Undo and Redo, in a bar of icons.
pub fn build_tools(board: &Whiteboard) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("whiteboard-tools");
    let tools = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    let mut first: Option<gtk::ToggleButton> = None;
    for (tool, tooltip) in [
        (Tool::Marker, "Marker"),
        (Tool::Arrow, "Arrow"),
        (Tool::Box, "Box"),
        (Tool::Sticky, "Sticky Note: click where it goes"),
        (Tool::Text, "Text: click where it goes"),
    ] {
        let icon = gtk::DrawingArea::builder().content_width(18).content_height(18).build();
        icon.set_draw_func(move |area, cr, width, height| draw_tool_icon(area, cr, tool, width, height));
        let button = gtk::ToggleButton::builder().child(&icon).tooltip_text(tooltip).build();
        button.add_css_class("flat");
        button.add_css_class("tool-button");
        button.set_group(first.as_ref());
        let board = board.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                board.set_tool(tool);
            }
        });
        tools.append(&button);
        first.get_or_insert(button);
    }
    if let Some(first) = first {
        first.set_active(true);
    }
    bar.append(&tools);
    bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let colors = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    colors.set_margin_start(4);
    colors.set_margin_end(4);
    let mut first: Option<gtk::ToggleButton> = None;
    for (name, color) in COLORS {
        let button = gtk::ToggleButton::builder().tooltip_text(name).valign(gtk::Align::Center).build();
        button.add_css_class("swatch");
        button.add_css_class(&format!("swatch-{}", name.to_lowercase()));
        button.set_group(first.as_ref());
        let board = board.clone();
        button.connect_toggled(move |button| {
            if button.is_active() {
                board.set_color(color);
            }
        });
        colors.append(&button);
        first.get_or_insert(button);
    }
    if let Some(first) = first {
        first.set_active(true);
    }
    bar.append(&colors);
    bar.append(&gtk::Separator::new(gtk::Orientation::Vertical));

    let undo = gtk::Button::builder().icon_name("edit-undo-symbolic").tooltip_text("Undo (Ctrl+Z)").build();
    let redo = gtk::Button::builder().icon_name("edit-redo-symbolic").tooltip_text("Redo (Ctrl+Shift+Z)").build();
    for (button, redo_it) in [(&undo, false), (&redo, true)] {
        button.add_css_class("flat");
        button.add_css_class("tool-button");
        let weak = board.weak();
        button.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                if redo_it { Whiteboard(inner).redo() } else { Whiteboard(inner).undo() }
            }
        });
        bar.append(button);
    }
    let (weak, buttons) = (board.weak(), (undo, redo));
    let update = move || {
        if let Some(inner) = weak.upgrade() {
            let board = Whiteboard(inner);
            buttons.0.set_sensitive(board.has_drawing());
            buttons.1.set_sensitive(board.can_redo());
        }
    };
    update();
    board.connect_change(update);

    let hint = util_hint();
    bar.append(&hint);
    bar
}

/// What to do next, at the end of the bar.
fn util_hint() -> gtk::Label {
    let hint = gtk::Label::builder()
        .label("Draw on the screenshot, then press Done · Esc discards it")
        .hexpand(true)
        .xalign(1.0)
        .ellipsize(pango::EllipsizeMode::End)
        .build();
    hint.add_css_class("dim-label");
    hint.add_css_class("caption");
    hint
}

/// Draws the icon of `tool` in the button's text colour.
fn draw_tool_icon(area: &gtk::DrawingArea, cr: &cairo::Context, tool: Tool, width: i32, height: i32) {
    let color = area.color();
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha()),
    );
    // Laid out on an 18 × 18 grid.
    let scale = f64::from(width.min(height)) / 18.0;
    cr.scale(scale, scale);
    cr.set_line_width(1.8);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Round);
    match tool {
        Tool::Marker => {
            cr.move_to(2.5, 13.0);
            cr.curve_to(5.0, 5.0, 7.5, 5.0, 9.0, 9.5);
            cr.curve_to(10.5, 14.0, 13.0, 14.0, 15.5, 5.0);
        }
        Tool::Arrow => {
            cr.move_to(3.0, 15.0);
            cr.line_to(15.0, 3.0);
            cr.move_to(8.0, 3.0);
            cr.line_to(15.0, 3.0);
            cr.line_to(15.0, 10.0);
        }
        Tool::Box => cr.rectangle(2.5, 4.0, 13.0, 10.0),
        Tool::Sticky => {
            cr.move_to(3.0, 3.0);
            cr.line_to(15.0, 3.0);
            cr.line_to(15.0, 11.0);
            cr.line_to(11.0, 15.0);
            cr.line_to(3.0, 15.0);
            cr.close_path();
            cr.move_to(15.0, 11.0);
            cr.line_to(11.0, 11.0);
            cr.line_to(11.0, 15.0);
        }
        Tool::Text => {
            cr.move_to(3.5, 4.0);
            cr.line_to(14.5, 4.0);
            cr.move_to(9.0, 4.0);
            cr.line_to(9.0, 15.0);
        }
    }
    let _ = cr.stroke();
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
        Item::Sticky { at, text } => draw_sticky(cr, *at, text, unit),
        Item::Text { color, at, text } => {
            let layout = text_layout(cr, text, "Sans Bold", 22.0 * unit, None);
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

/// A square note of yellow paper, a little askew, its corner curling up,
/// with `text` on it.
fn draw_sticky(cr: &cairo::Context, at: (f64, f64), text: &str, unit: f64) {
    let (side, pad, curl) = (170.0 * unit, 16.0 * unit, 22.0 * unit);
    let layout = text_layout(cr, text, "Sans", 17.0 * unit, Some(side - 2.0 * pad));
    let height = side.max(f64::from(layout.pixel_size().1) + 2.0 * pad + curl);
    cr.translate(at.0 + side / 2.0, at.1 + height / 2.0);
    cr.rotate(-0.035);
    cr.translate(-side / 2.0, -height / 2.0);
    // The outline, without the corner that curls up.
    let paper = |cr: &cairo::Context, dx: f64, dy: f64| {
        cr.move_to(dx, dy);
        cr.line_to(side + dx, dy);
        cr.line_to(side + dx, height - curl + dy);
        cr.line_to(side - curl + dx, height + dy);
        cr.line_to(dx, height + dy);
        cr.close_path();
    };
    // A soft shadow, deeper towards the bottom.
    for step in 1..=5 {
        let offset = f64::from(step) * unit;
        paper(cr, offset * 0.6, offset * 1.2);
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.045);
        let _ = cr.fill();
    }
    paper(cr, 0.0, 0.0);
    let gradient = cairo::LinearGradient::new(0.0, 0.0, 0.0, height);
    gradient.add_color_stop_rgb(0.0, STICKY_TOP.0, STICKY_TOP.1, STICKY_TOP.2);
    gradient.add_color_stop_rgb(1.0, STICKY_BOTTOM.0, STICKY_BOTTOM.1, STICKY_BOTTOM.2);
    let _ = cr.set_source(&gradient);
    let _ = cr.fill();
    // The sticky strip along the top.
    cr.rectangle(0.0, 0.0, side, 18.0 * unit);
    cr.set_source_rgba(0.55, 0.42, 0.0, 0.07);
    let _ = cr.fill();
    // The curled corner.
    cr.move_to(side, height - curl);
    cr.line_to(side - curl * 0.85, height - curl * 0.85);
    cr.line_to(side - curl, height);
    cr.close_path();
    set_color(cr, STICKY_CURL);
    let _ = cr.fill();
    cr.set_source_rgb(0.27, 0.22, 0.04);
    cr.move_to(pad, pad + 6.0 * unit);
    pangocairo::functions::show_layout(cr, &layout);
}

fn set_color(cr: &cairo::Context, (r, g, b): Rgb) {
    cr.set_source_rgb(r, g, b);
}

fn text_layout(cr: &cairo::Context, text: &str, font: &str, size: f64, wrap: Option<f64>) -> pango::Layout {
    let layout = pangocairo::functions::create_layout(cr);
    let mut font = pango::FontDescription::from_string(font);
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
