use gtk4::prelude::*;
use gtk4::{self as gtk, gdk, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Show a fullscreen image viewer with pan and zoom.
pub fn show(texture: &gdk::Texture, parent: &impl IsA<gtk::Window>) {
    let window = gtk::Window::builder()
        .title("Image")
        .transient_for(parent)
        .modal(true)
        .fullscreened(true)
        .build();

    let img_w = texture.width() as f64;
    let img_h = texture.height() as f64;

    // Shared texture ref — cleared on close to release memory
    let texture_ref: Rc<RefCell<Option<gdk::Texture>>> = Rc::new(RefCell::new(Some(texture.clone())));

    // Initial zoom computed on first draw to fit image to screen
    let zoom = Rc::new(Cell::new(0.0_f64)); // 0 = not yet computed
    let offset_x = Rc::new(Cell::new(0.0_f64));
    let offset_y = Rc::new(Cell::new(0.0_f64));

    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);

    // Draw
    {
        let tex = texture_ref.clone();
        let zoom = zoom.clone();
        let ox = offset_x.clone();
        let oy = offset_y.clone();
        area.set_draw_func(move |_area, cr, width, height| {
            cr.set_source_rgb(0.0, 0.0, 0.0);
            cr.paint().ok();

            let tex_borrow = tex.borrow();
            let Some(texture) = tex_borrow.as_ref() else { return };

            // Compute initial zoom to fit image to screen on first draw
            let mut z = zoom.get();
            if z == 0.0 && width > 0 && height > 0 && img_w > 0.0 && img_h > 0.0 {
                let fit_w = width as f64 / img_w;
                let fit_h = height as f64 / img_h;
                z = fit_w.min(fit_h);
                zoom.set(z);
            }
            let scaled_w = img_w * z;
            let scaled_h = img_h * z;

            let cx = (width as f64 - scaled_w) / 2.0 + ox.get();
            let cy = (height as f64 - scaled_h) / 2.0 + oy.get();

            cr.translate(cx, cy);
            cr.scale(z, z);

            let snapshot = gtk::Snapshot::new();
            texture.snapshot(snapshot.upcast_ref::<gdk::Snapshot>(), img_w, img_h);
            if let Some(node) = snapshot.to_node() {
                node.draw(cr);
            }
        });
    }

    // Zoom with scroll
    {
        let scroll_ctrl = gtk::EventControllerScroll::new(
            gtk::EventControllerScrollFlags::VERTICAL,
        );
        let zoom = zoom.clone();
        let area_weak = area.downgrade();
        scroll_ctrl.connect_scroll(move |_, _dx, dy| {
            let factor = if dy < 0.0 { 1.1 } else { 1.0 / 1.1 };
            let new_zoom = (zoom.get() * factor).clamp(0.1, 20.0);
            zoom.set(new_zoom);
            if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
            glib::Propagation::Stop
        });
        area.add_controller(scroll_ctrl);
    }

    // Pan with drag
    {
        let drag = gtk::GestureDrag::new();
        let ox = offset_x.clone();
        let oy = offset_y.clone();
        let start_ox = Rc::new(Cell::new(0.0_f64));
        let start_oy = Rc::new(Cell::new(0.0_f64));
        let area_weak = area.downgrade();

        let sox = start_ox.clone();
        let soy = start_oy.clone();
        let ox2 = ox.clone();
        let oy2 = oy.clone();
        drag.connect_drag_begin(move |_, _, _| {
            sox.set(ox2.get());
            soy.set(oy2.get());
        });

        let sox = start_ox;
        let soy = start_oy;
        drag.connect_drag_update(move |_, dx, dy| {
            ox.set(sox.get() + dx);
            oy.set(soy.get() + dy);
            if let Some(a) = area_weak.upgrade() { a.queue_draw(); }
        });

        area.add_controller(drag);
    }

    // Close on Escape
    {
        let key_ctrl = gtk::EventControllerKey::new();
        let w = window.downgrade();
        key_ctrl.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(w) = w.upgrade() { w.close(); }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        window.add_controller(key_ctrl);
    }

    // Close button overlay
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&area));

    let close_btn = gtk::Button::from_icon_name("window-close-symbolic");
    close_btn.add_css_class("osd");
    close_btn.add_css_class("circular");
    close_btn.set_halign(gtk::Align::End);
    close_btn.set_valign(gtk::Align::Start);
    close_btn.set_margin_top(16);
    close_btn.set_margin_end(16);
    let w = window.downgrade();
    close_btn.connect_clicked(move |_| {
        if let Some(w) = w.upgrade() { w.close(); }
    });
    overlay.add_overlay(&close_btn);

    // Double-click to reset zoom/pan
    {
        let click = gtk::GestureClick::new();
        click.set_button(1);
        let zoom = zoom.clone();
        let ox = offset_x.clone();
        let oy = offset_y.clone();
        let area_weak = area.downgrade();
        click.connect_released(move |gesture, n_press, _, _| {
            if n_press == 2 {
                zoom.set(0.0); // reset to fit-to-screen (recomputed on next draw)
                ox.set(0.0);
                oy.set(0.0);
                if let Some(area) = area_weak.upgrade() { area.queue_draw(); }
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        area.add_controller(click);
    }

    // On close, drop the texture and tear down widget tree
    let tex_close = texture_ref;
    window.connect_close_request(move |win| {
        // Release the texture immediately
        *tex_close.borrow_mut() = None;
        win.set_child(None::<&gtk::Widget>);
        glib::Propagation::Proceed
    });

    window.set_child(Some(&overlay));
    window.present();
}
