//! Shared-workspace layout planner (GTK DrawingArea).
//!
//! Authority drags rigid host monitor groups in continuous X/Y workspace
//! space; followers are view-only. Daemon owns Apply validation and sync.

use adw::prelude::*;
use gtk::cairo::{Context, LineCap};
use gtk::glib;
use gtk::{DrawingArea, GestureClick, GestureDrag, Orientation, SpinButton};
use input_event::display::DisplayRect;
use input_event::workspace::{
    DevicePlacement, DisplayId, LocalDisplay, LocalTopology, WorkspaceConfig, derive_routes,
    snap_device_origin,
};
use mousehop_ipc::{
    FrontendRequest, WorkspaceDeviceIpc, WorkspaceDisplayIpc, WorkspaceIpc, WorkspacePlacementIpc,
    WorkspaceRouteIpc,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

const SNAP_THRESHOLD: i32 = 24;
const PLACEHOLDER_W: u32 = 1920;
const PLACEHOLDER_H: u32 = 1080;
/// Minimum DrawingArea allocation before fit_view may run (avoids scale 0.01).
const FIT_MIN_PX: i32 = 64;
const PEER_SIDEBAR_WIDTH: i32 = 280;

#[derive(Clone)]
struct ViewTransform {
    scale: f64,
    origin_x: f64,
    origin_y: f64,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self {
            scale: 0.08,
            origin_x: 40.0,
            origin_y: 40.0,
        }
    }
}

impl ViewTransform {
    fn world_to_view(&self, x: i32, y: i32) -> (f64, f64) {
        (
            f64::from(x) * self.scale + self.origin_x,
            f64::from(y) * self.scale + self.origin_y,
        )
    }

    fn view_to_world(&self, x: f64, y: f64) -> (i32, i32) {
        let wx = ((x - self.origin_x) / self.scale).round() as i32;
        let wy = ((y - self.origin_y) / self.scale).round() as i32;
        (wx, wy)
    }
}

struct EditorState {
    committed: WorkspaceIpc,
    draft: WorkspaceIpc,
    selected: Option<String>,
    snap: bool,
    zoom: f64,
    view: ViewTransform,
    error: String,
    /// Fingerprint being dragged, with grab offset in world coords.
    drag: Option<(String, i32, i32)>,
    /// Fit once the DrawingArea has a real allocation.
    needs_fit: bool,
}

pub fn present_workspace_editor(
    parent: &impl IsA<gtk::Window>,
    request: Rc<dyn Fn(FrontendRequest)>,
    initial: WorkspaceIpc,
    register_live: Rc<dyn Fn(Option<Rc<dyn Fn(WorkspaceIpc)>>)>,
) {
    let dialog = adw::Window::new();
    dialog.set_transient_for(Some(parent));
    dialog.set_modal(true);
    dialog.set_title(Some("Shared layout"));
    dialog.set_default_size(920, 640);

    let editable = initial.is_authority;
    let state = Rc::new(RefCell::new(EditorState {
        committed: initial.clone(),
        draft: initial.clone(),
        selected: initial.placements.first().map(|p| p.fingerprint.clone()),
        snap: true,
        zoom: 1.0,
        view: ViewTransform::default(),
        error: String::new(),
        drag: None,
        needs_fit: true,
    }));

    let root = gtk::Box::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let banner = gtk::Label::new(Some(if editable {
        "You are the workspace authority. Drag host groups freely in X/Y, then Apply."
    } else {
        "Follower (view-only). The authority publishes the shared layout."
    }));
    banner.set_wrap(true);
    banner.set_xalign(0.0);
    root.append(&banner);

    let meta = gtk::Label::new(None);
    meta.add_css_class("dim-label");
    meta.set_xalign(0.0);
    root.append(&meta);

    let error_banner = gtk::Label::new(None);
    error_banner.add_css_class("error");
    error_banner.set_wrap(true);
    error_banner.set_xalign(0.0);
    error_banner.set_visible(false);
    root.append(&error_banner);

    let body = gtk::Box::new(Orientation::Horizontal, 10);
    body.set_vexpand(true);
    body.set_hexpand(true);

    let peer_scroller = gtk::ScrolledWindow::new();
    peer_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    peer_scroller.set_min_content_width(PEER_SIDEBAR_WIDTH);
    peer_scroller.set_size_request(PEER_SIDEBAR_WIDTH, -1);
    peer_scroller.set_hexpand(false);
    peer_scroller.set_vexpand(true);
    let peer_list = gtk::ListBox::new();
    peer_list.add_css_class("boxed-list");
    peer_list.set_selection_mode(gtk::SelectionMode::None);
    peer_scroller.set_child(Some(&peer_list));
    body.append(&peer_scroller);

    let canvas_col = gtk::Box::new(Orientation::Vertical, 6);
    canvas_col.set_hexpand(true);
    canvas_col.set_vexpand(true);

    let toolbar = gtk::Box::new(Orientation::Horizontal, 8);
    let snap_toggle = gtk::ToggleButton::with_label("Snap");
    snap_toggle.set_active(true);
    snap_toggle.set_sensitive(editable);
    toolbar.append(&snap_toggle);

    let fit_btn = gtk::Button::with_label("Fit");
    toolbar.append(&fit_btn);
    let zoom_out = gtk::Button::with_label("−");
    toolbar.append(&zoom_out);
    let zoom_in = gtk::Button::with_label("+");
    toolbar.append(&zoom_in);

    toolbar.append(&gtk::Label::new(Some("X")));
    let spin_x = SpinButton::with_range(-50000.0, 50000.0, 10.0);
    spin_x.set_digits(0);
    spin_x.set_sensitive(editable);
    toolbar.append(&spin_x);
    toolbar.append(&gtk::Label::new(Some("Y")));
    let spin_y = SpinButton::with_range(-50000.0, 50000.0, 10.0);
    spin_y.set_digits(0);
    spin_y.set_sensitive(editable);
    toolbar.append(&spin_y);
    canvas_col.append(&toolbar);

    let area = DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    area.set_content_width(640);
    area.set_content_height(420);
    area.set_draw_func(glib::clone!(
        #[strong]
        state,
        move |_, cr, width, height| {
            if state.borrow().needs_fit {
                fit_view(&state, width, height);
            }
            draw_canvas(cr, width, height, &state.borrow());
        }
    ));
    canvas_col.append(&area);
    body.append(&canvas_col);
    root.append(&body);

    let buttons = gtk::Box::new(Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let close = gtk::Button::with_label("Close");
    close.connect_clicked(glib::clone!(
        #[strong]
        dialog,
        move |_| dialog.close()
    ));
    buttons.append(&close);

    let claim = gtk::Button::with_label("Claim authority");
    claim.connect_clicked(glib::clone!(
        #[strong]
        request,
        move |_| {
            request(FrontendRequest::SetWorkspaceAuthority("self".into()));
        }
    ));
    buttons.append(&claim);

    let revert = gtk::Button::with_label("Revert");
    revert.set_sensitive(editable);
    buttons.append(&revert);

    let apply = gtk::Button::with_label("Apply");
    apply.add_css_class("suggested-action");
    apply.set_sensitive(editable);
    buttons.append(&apply);
    root.append(&buttons);

    dialog.set_content(Some(&root));

    let refresh_ui_slot: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let refresh_ui: Rc<dyn Fn()> = {
        let state = state.clone();
        let meta = meta.clone();
        let error_banner = error_banner.clone();
        let peer_list = peer_list.clone();
        let area = area.clone();
        let spin_x = spin_x.clone();
        let spin_y = spin_y.clone();
        let refresh_ui_slot = refresh_ui_slot.clone();
        Rc::new(move || {
            let st = state.borrow();
            meta.set_text(&format!(
                "workspace={}  revision={}  authority={}",
                if st.draft.workspace_id.is_empty() {
                    "(none)"
                } else {
                    &st.draft.workspace_id
                },
                st.draft.revision,
                trunc(&st.draft.authority_fingerprint, 12)
            ));
            if st.error.is_empty() {
                error_banner.set_visible(false);
                error_banner.set_text("");
            } else {
                error_banner.set_visible(true);
                error_banner.set_text(&st.error);
            }
            while let Some(child) = peer_list.first_child() {
                peer_list.remove(&child);
            }
            drop(st);
            let refresh = refresh_ui_slot.borrow().clone();
            rebuild_peer_list(&peer_list, &state, refresh.as_ref());
            sync_spins(&state, &spin_x, &spin_y);
            area.queue_draw();
        })
    };
    *refresh_ui_slot.borrow_mut() = Some(refresh_ui.clone());

    refresh_ui();
    area.queue_draw();

    snap_toggle.connect_toggled(glib::clone!(
        #[strong]
        state,
        move |btn| {
            state.borrow_mut().snap = btn.is_active();
        }
    ));

    fit_btn.connect_clicked(glib::clone!(
        #[strong]
        state,
        #[strong]
        area,
        move |_| {
            state.borrow_mut().needs_fit = true;
            fit_view(&state, area.width(), area.height());
            area.queue_draw();
        }
    ));
    zoom_in.connect_clicked(glib::clone!(
        #[strong]
        state,
        #[strong]
        area,
        move |_| {
            {
                let mut st = state.borrow_mut();
                st.zoom = (st.zoom * 1.2).min(8.0);
                st.needs_fit = true;
            }
            fit_view(&state, area.width(), area.height());
            area.queue_draw();
        }
    ));
    zoom_out.connect_clicked(glib::clone!(
        #[strong]
        state,
        #[strong]
        area,
        move |_| {
            {
                let mut st = state.borrow_mut();
                st.zoom = (st.zoom / 1.2).max(0.25);
                st.needs_fit = true;
            }
            fit_view(&state, area.width(), area.height());
            area.queue_draw();
        }
    ));

    spin_x.connect_value_changed(glib::clone!(
        #[strong]
        state,
        #[strong]
        area,
        #[strong]
        refresh_ui,
        move |spin| {
            let mut st = state.borrow_mut();
            let Some(fp) = st.selected.clone() else {
                return;
            };
            if let Some(p) = st.draft.placements.iter_mut().find(|p| p.fingerprint == fp) {
                p.origin_x = spin.value() as i32;
            }
            st.error = validate_draft(&st.draft);
            drop(st);
            refresh_preview_routes(&state);
            area.queue_draw();
            let _ = refresh_ui;
        }
    ));
    spin_y.connect_value_changed(glib::clone!(
        #[strong]
        state,
        #[strong]
        area,
        move |spin| {
            let mut st = state.borrow_mut();
            let Some(fp) = st.selected.clone() else {
                return;
            };
            if let Some(p) = st.draft.placements.iter_mut().find(|p| p.fingerprint == fp) {
                p.origin_y = spin.value() as i32;
            }
            st.error = validate_draft(&st.draft);
            drop(st);
            refresh_preview_routes(&state);
            area.queue_draw();
        }
    ));

    if editable {
        let click = GestureClick::new();
        click.connect_pressed(glib::clone!(
            #[strong]
            state,
            #[strong]
            area,
            #[strong]
            refresh_ui,
            move |_, _n, x, y| {
                let mut st = state.borrow_mut();
                let (wx, wy) = st.view.view_to_world(x, y);
                if let Some(fp) = hit_test(&st.draft, wx, wy) {
                    st.selected = Some(fp);
                }
                drop(st);
                refresh_ui();
                area.queue_draw();
            }
        ));
        area.add_controller(click);

        let drag = GestureDrag::new();
        drag.connect_drag_begin(glib::clone!(
            #[strong]
            state,
            move |gesture, x, y| {
                let mut st = state.borrow_mut();
                let (wx, wy) = st.view.view_to_world(x, y);
                let Some(fp) = hit_test(&st.draft, wx, wy) else {
                    gesture.set_state(gtk::EventSequenceState::Denied);
                    return;
                };
                st.selected = Some(fp.clone());
                let origin = st
                    .draft
                    .placements
                    .iter()
                    .find(|p| p.fingerprint == fp)
                    .map(|p| (p.origin_x, p.origin_y))
                    .unwrap_or((0, 0));
                st.drag = Some((fp, wx - origin.0, wy - origin.1));
            }
        ));
        drag.connect_drag_update(glib::clone!(
            #[strong]
            state,
            #[strong]
            area,
            #[strong]
            spin_x,
            #[strong]
            spin_y,
            move |gesture, dx, dy| {
                let Some((start_x, start_y)) = gesture.start_point() else {
                    return;
                };
                let mut st = state.borrow_mut();
                let Some((fp, ox, oy)) = st.drag.clone() else {
                    return;
                };
                let (wx, wy) = st.view.view_to_world(start_x + dx, start_y + dy);
                if let Some(p) = st.draft.placements.iter_mut().find(|p| p.fingerprint == fp) {
                    p.origin_x = wx - ox;
                    p.origin_y = wy - oy;
                }
                drop(st);
                sync_spins(&state, &spin_x, &spin_y);
                area.queue_draw();
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[strong]
            state,
            #[strong]
            area,
            #[strong]
            refresh_ui,
            move |_, _dx, _dy| {
                {
                    let mut st = state.borrow_mut();
                    if let Some((fp, _, _)) = st.drag.take() {
                        if st.snap {
                            apply_snap(&mut st.draft, &fp);
                        }
                        st.error = validate_draft(&st.draft);
                    }
                }
                refresh_preview_routes(&state);
                refresh_ui();
                area.queue_draw();
            }
        ));
        area.add_controller(drag);
    }

    revert.connect_clicked(glib::clone!(
        #[strong]
        state,
        #[strong]
        refresh_ui,
        move |_| {
            {
                let mut st = state.borrow_mut();
                st.draft = st.committed.clone();
                st.error.clear();
            }
            refresh_preview_routes(&state);
            refresh_ui();
        }
    ));

    apply.connect_clicked(glib::clone!(
        #[strong]
        state,
        #[strong]
        request,
        #[strong]
        refresh_ui,
        move |_| {
            let err = {
                let st = state.borrow();
                validate_draft(&st.draft)
            };
            if !err.is_empty() {
                state.borrow_mut().error = err;
                refresh_ui();
                return;
            }
            let mut ws = state.borrow().draft.clone();
            if ws.workspace_id.is_empty() {
                ws.workspace_id = "default".into();
            }
            if ws.placements.is_empty() {
                ws.placements.push(WorkspacePlacementIpc {
                    fingerprint: ws.authority_fingerprint.clone(),
                    origin_x: 0,
                    origin_y: 0,
                });
            }
            state.borrow_mut().committed = ws.clone();
            request(FrontendRequest::ApplyWorkspace(ws));
            refresh_ui();
        }
    ));

    let live_hook: Rc<dyn Fn(WorkspaceIpc)> = Rc::new({
        let state = state.clone();
        let refresh_ui = refresh_ui.clone();
        move |incoming: WorkspaceIpc| {
            merge_incoming_workspace(&state, incoming);
            refresh_ui();
        }
    });
    register_live(Some(live_hook));
    dialog.connect_close_request(glib::clone!(
        #[strong]
        register_live,
        move |_| {
            register_live(None);
            glib::Propagation::Proceed
        }
    ));

    dialog.present();
}

/// Merge a daemon workspace snapshot into the open editor.
///
/// If the draft is clean (`draft == committed`), replace both. If the user has
/// unsaved moves, only append placements/devices for new fingerprints.
fn merge_incoming_workspace(state: &Rc<RefCell<EditorState>>, incoming: WorkspaceIpc) {
    let mut st = state.borrow_mut();
    let dirty = st.draft != st.committed;
    if !dirty {
        st.committed = incoming.clone();
        st.draft = incoming;
        st.needs_fit = true;
        if st.selected.is_none() {
            st.selected = st.draft.placements.first().map(|p| p.fingerprint.clone());
        }
        return;
    }
    // Keep draft origins; refresh committed snapshot and merge new peers.
    st.committed = incoming.clone();
    for p in &incoming.placements {
        if st
            .draft
            .placements
            .iter()
            .any(|d| d.fingerprint == p.fingerprint)
        {
            continue;
        }
        st.draft.placements.push(p.clone());
    }
    for d in &incoming.devices {
        if let Some(existing) = st
            .draft
            .devices
            .iter_mut()
            .find(|x| x.fingerprint == d.fingerprint)
        {
            *existing = d.clone();
        } else {
            st.draft.devices.push(d.clone());
        }
    }
}

fn rebuild_peer_list(
    list: &gtk::ListBox,
    state: &Rc<RefCell<EditorState>>,
    refresh_ui: Option<&Rc<dyn Fn()>>,
) {
    let st = state.borrow();
    for placement in &st.draft.placements {
        let device = st
            .draft
            .devices
            .iter()
            .find(|d| d.fingerprint == placement.fingerprint);
        let label = device
            .map(|d| d.label.as_str())
            .unwrap_or(placement.fingerprint.as_str());
        let presence = device.map(|d| d.presence.as_str()).unwrap_or("offline");
        let selected = st.selected.as_deref() == Some(placement.fingerprint.as_str());

        let row = adw::ExpanderRow::new();
        row.set_title(label);
        row.set_subtitle(&format!(
            "{presence} · origin ({}, {}){}",
            placement.origin_x,
            placement.origin_y,
            if selected { " · selected" } else { "" }
        ));
        if selected {
            row.add_css_class("accent");
        }
        let fp = placement.fingerprint.clone();
        if let Some(refresh_ui) = refresh_ui {
            let select = GestureClick::new();
            select.connect_pressed(glib::clone!(
                #[strong]
                state,
                #[strong]
                refresh_ui,
                #[strong]
                fp,
                move |_, _, _, _| {
                    if state.borrow().selected.as_deref() == Some(fp.as_str()) {
                        return;
                    }
                    state.borrow_mut().selected = Some(fp.clone());
                    let refresh_ui = refresh_ui.clone();
                    glib::idle_add_local_once(move || {
                        refresh_ui();
                    });
                }
            ));
            row.add_controller(select);
        }
        if let Some(device) = device {
            for display in &device.displays {
                let drow = adw::ActionRow::new();
                drow.set_title(&display.id);
                drow.set_subtitle(&format!(
                    "{}×{} @ ({}, {})",
                    display.width, display.height, display.x, display.y
                ));
                drow.set_sensitive(false);
                row.add_row(&drow);
            }
            if device.displays.is_empty() {
                let drow = adw::ActionRow::new();
                drow.set_title("(no topology yet)");
                drow.set_sensitive(false);
                row.add_row(&drow);
            }
        }
        list.append(&row);
    }
}

fn sync_spins(state: &Rc<RefCell<EditorState>>, spin_x: &SpinButton, spin_y: &SpinButton) {
    let st = state.borrow();
    let Some(fp) = st.selected.as_ref() else {
        return;
    };
    let Some(p) = st.draft.placements.iter().find(|p| p.fingerprint == *fp) else {
        return;
    };
    if (spin_x.value() - f64::from(p.origin_x)).abs() > f64::EPSILON {
        spin_x.set_value(f64::from(p.origin_x));
    }
    if (spin_y.value() - f64::from(p.origin_y)).abs() > f64::EPSILON {
        spin_y.set_value(f64::from(p.origin_y));
    }
}

fn host_rects(ws: &WorkspaceIpc, fingerprint: &str) -> Vec<(i32, i32, u32, u32)> {
    let origin = ws
        .placements
        .iter()
        .find(|p| p.fingerprint == fingerprint)
        .map(|p| (p.origin_x, p.origin_y))
        .unwrap_or((0, 0));
    let displays = ws
        .devices
        .iter()
        .find(|d| d.fingerprint == fingerprint)
        .map(|d| d.displays.as_slice())
        .unwrap_or(&[]);
    if displays.is_empty() {
        return vec![(origin.0, origin.1, PLACEHOLDER_W, PLACEHOLDER_H)];
    }
    displays
        .iter()
        .map(|d| (origin.0 + d.x, origin.1 + d.y, d.width, d.height))
        .collect()
}

fn hit_test(ws: &WorkspaceIpc, wx: i32, wy: i32) -> Option<String> {
    for p in ws.placements.iter().rev() {
        for (x, y, w, h) in host_rects(ws, &p.fingerprint) {
            if wx >= x && wy >= y && wx < x + w as i32 && wy < y + h as i32 {
                return Some(p.fingerprint.clone());
            }
        }
    }
    None
}

fn fit_view(state: &Rc<RefCell<EditorState>>, width: i32, height: i32) {
    if width < FIT_MIN_PX || height < FIT_MIN_PX {
        return;
    }
    let mut st = state.borrow_mut();
    let mut min_x = i32::MAX;
    let mut min_y = i32::MAX;
    let mut max_x = i32::MIN;
    let mut max_y = i32::MIN;
    let fps: Vec<_> = st
        .draft
        .placements
        .iter()
        .map(|p| p.fingerprint.clone())
        .collect();
    for fp in &fps {
        for (x, y, w, h) in host_rects(&st.draft, fp) {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x + w as i32);
            max_y = max_y.max(y + h as i32);
        }
    }
    if min_x == i32::MAX {
        st.view = ViewTransform::default();
        st.view.scale *= st.zoom;
        st.needs_fit = false;
        return;
    }
    let world_w = (max_x - min_x).max(1) as f64;
    let world_h = (max_y - min_y).max(1) as f64;
    let pad = 48.0;
    let sx = (width as f64 - pad * 2.0) / world_w;
    let sy = (height as f64 - pad * 2.0) / world_h;
    let scale = sx.min(sy).clamp(0.01, 2.0) * st.zoom;
    st.view.scale = scale;
    st.view.origin_x = pad - f64::from(min_x) * scale;
    st.view.origin_y = pad - f64::from(min_y) * scale;
    st.needs_fit = false;
}

fn draw_canvas(cr: &Context, width: i32, height: i32, st: &EditorState) {
    cr.set_source_rgb(0.96, 0.96, 0.97);
    cr.rectangle(0.0, 0.0, f64::from(width), f64::from(height));
    let _ = cr.fill();

    // Grid
    cr.set_source_rgba(0.0, 0.0, 0.0, 0.06);
    cr.set_line_width(1.0);
    let step = (100.0 * st.view.scale).max(16.0);
    let mut x = st.view.origin_x % step;
    while x < f64::from(width) {
        cr.move_to(x, 0.0);
        cr.line_to(x, f64::from(height));
        x += step;
    }
    let mut y = st.view.origin_y % step;
    while y < f64::from(height) {
        cr.move_to(0.0, y);
        cr.line_to(f64::from(width), y);
        y += step;
    }
    let _ = cr.stroke();

    for (idx, p) in st.draft.placements.iter().enumerate() {
        let selected = st.selected.as_deref() == Some(p.fingerprint.as_str());
        let device = st
            .draft
            .devices
            .iter()
            .find(|d| d.fingerprint == p.fingerprint);
        let (r, g, b) = host_color(
            idx,
            device.map(|d| d.presence.as_str()).unwrap_or("offline"),
        );
        for (hx, hy, w, h) in host_rects(&st.draft, &p.fingerprint) {
            let (vx, vy) = st.view.world_to_view(hx, hy);
            let vw = f64::from(w) * st.view.scale;
            let vh = f64::from(h) * st.view.scale;
            cr.set_source_rgba(r, g, b, if selected { 0.55 } else { 0.35 });
            cr.rectangle(vx, vy, vw, vh);
            let _ = cr.fill();
            cr.set_source_rgba(r, g, b, 1.0);
            cr.set_line_width(if selected { 2.5 } else { 1.2 });
            cr.rectangle(vx, vy, vw, vh);
            let _ = cr.stroke();
        }
        if let Some((hx, hy, w, h)) = host_rects(&st.draft, &p.fingerprint).first().copied() {
            let (vx, vy) = st.view.world_to_view(hx, hy);
            let vh = f64::from(h) * st.view.scale;
            // Fixed baseline (+16) needs enough rect height to avoid clipped glyphs.
            if vh >= 24.0 {
                let label = device
                    .map(|d| d.label.as_str())
                    .unwrap_or(p.fingerprint.as_str());
                cr.set_source_rgb(0.15, 0.15, 0.18);
                cr.move_to(vx + 6.0, vy + 16.0);
                let _ = cr.show_text(&trunc(label, 18));
            }
            let _ = w;
        }
    }

    // Route highlights in workspace coordinates.
    cr.set_line_width(3.0);
    cr.set_line_cap(LineCap::Round);
    cr.set_source_rgba(0.15, 0.45, 0.85, 0.85);
    for route in &st.draft.routes {
        if let Some(seg) = route_segment_view(&st.draft, route, &st.view) {
            cr.move_to(seg.0, seg.1);
            cr.line_to(seg.2, seg.3);
            let _ = cr.stroke();
        }
    }
}

fn route_segment_view(
    ws: &WorkspaceIpc,
    route: &WorkspaceRouteIpc,
    view: &ViewTransform,
) -> Option<(f64, f64, f64, f64)> {
    let origin = ws
        .placements
        .iter()
        .find(|p| p.fingerprint == route.src_fingerprint)
        .map(|p| (p.origin_x, p.origin_y))?;
    let display = ws
        .devices
        .iter()
        .find(|d| d.fingerprint == route.src_fingerprint)?
        .displays
        .iter()
        .find(|d| d.id == route.src_display_id)?;
    let x0 = origin.0 + display.x;
    let y0 = origin.1 + display.y;
    let (ax, ay, bx, by) = match route.edge.as_str() {
        "right" => (
            x0 + display.width as i32,
            y0 + route.start,
            x0 + display.width as i32,
            y0 + route.end,
        ),
        "left" => (x0, y0 + route.start, x0, y0 + route.end),
        "bottom" => (
            x0 + route.start,
            y0 + display.height as i32,
            x0 + route.end,
            y0 + display.height as i32,
        ),
        "top" => (x0 + route.start, y0, x0 + route.end, y0),
        _ => return None,
    };
    let (vx0, vy0) = view.world_to_view(ax, ay);
    let (vx1, vy1) = view.world_to_view(bx, by);
    Some((vx0, vy0, vx1, vy1))
}

fn host_color(idx: usize, presence: &str) -> (f64, f64, f64) {
    if presence == "offline" {
        return (0.55, 0.55, 0.58);
    }
    const PALETTE: [(f64, f64, f64); 5] = [
        (0.20, 0.45, 0.75),
        (0.15, 0.55, 0.40),
        (0.70, 0.40, 0.15),
        (0.55, 0.25, 0.55),
        (0.35, 0.50, 0.20),
    ];
    PALETTE[idx % PALETTE.len()]
}

fn topologies_from_ipc(ws: &WorkspaceIpc) -> BTreeMap<String, LocalTopology> {
    let mut map = BTreeMap::new();
    for device in &ws.devices {
        let displays = if device.displays.is_empty() {
            vec![LocalDisplay {
                id: DisplayId::new("placeholder").expect("id"),
                rect: DisplayRect::new(0, 0, PLACEHOLDER_W, PLACEHOLDER_H).expect("rect"),
                scale: None,
                rotation: None,
            }]
        } else {
            device
                .displays
                .iter()
                .filter_map(display_from_ipc)
                .collect()
        };
        if let Ok(topo) = LocalTopology::try_new(displays, 1, 1) {
            map.insert(device.fingerprint.clone(), topo);
        }
    }
    map
}

fn display_from_ipc(d: &WorkspaceDisplayIpc) -> Option<LocalDisplay> {
    Some(LocalDisplay {
        id: DisplayId::new(d.id.clone())?,
        rect: DisplayRect::new(d.x, d.y, d.width, d.height)?,
        scale: None,
        rotation: None,
    })
}

fn draft_config(ws: &WorkspaceIpc) -> WorkspaceConfig {
    WorkspaceConfig {
        workspace_id: if ws.workspace_id.is_empty() {
            "default".into()
        } else {
            ws.workspace_id.clone()
        },
        revision: ws.revision.max(1),
        authority_fingerprint: if ws.authority_fingerprint.is_empty() {
            "self".into()
        } else {
            ws.authority_fingerprint.clone()
        },
        placements: ws
            .placements
            .iter()
            .map(|p| DevicePlacement {
                peer_fingerprint: p.fingerprint.clone(),
                origin_in_workspace: (p.origin_x, p.origin_y),
            })
            .collect(),
    }
}

fn validate_draft(ws: &WorkspaceIpc) -> String {
    let config = draft_config(ws);
    let topologies = topologies_from_ipc(ws);
    match derive_routes(&config, &topologies) {
        Ok(_) => String::new(),
        Err(e) => e.to_string(),
    }
}

fn refresh_preview_routes(state: &Rc<RefCell<EditorState>>) {
    let mut st = state.borrow_mut();
    let config = draft_config(&st.draft);
    let topologies = topologies_from_ipc(&st.draft);
    match derive_routes(&config, &topologies) {
        Ok(snap) => {
            st.draft.routes = snap
                .routes()
                .iter()
                .map(|r| WorkspaceRouteIpc {
                    src_fingerprint: r.src_peer.clone(),
                    src_display_id: r.src_display.as_str().to_string(),
                    edge: format!("{:?}", r.src_edge).to_lowercase(),
                    start: r.src_start,
                    end: r.src_end,
                    dst_fingerprint: r.dst_peer.clone(),
                    dst_display_id: r.dst_display.as_str().to_string(),
                })
                .collect();
            st.error.clear();
        }
        Err(e) => {
            st.draft.routes.clear();
            st.error = e.to_string();
        }
    }
}

fn apply_snap(ws: &mut WorkspaceIpc, moving_fp: &str) {
    let topologies = topologies_from_ipc(ws);
    let Some(moving_topo) = topologies.get(moving_fp).cloned() else {
        return;
    };
    let Some(moving) = ws
        .placements
        .iter()
        .find(|p| p.fingerprint == moving_fp)
        .map(|p| (p.origin_x, p.origin_y))
    else {
        return;
    };
    let mut best = moving;
    let mut best_dist = u32::MAX;
    for other in &ws.placements {
        if other.fingerprint == moving_fp {
            continue;
        }
        let Some(anchor_topo) = topologies.get(&other.fingerprint) else {
            continue;
        };
        let snapped = snap_device_origin(
            moving,
            &moving_topo,
            (other.origin_x, other.origin_y),
            anchor_topo,
            SNAP_THRESHOLD,
        );
        let dist = (snapped.0 - moving.0).unsigned_abs() + (snapped.1 - moving.1).unsigned_abs();
        if dist < best_dist {
            best_dist = dist;
            best = snapped;
        }
    }
    if let Some(p) = ws
        .placements
        .iter_mut()
        .find(|p| p.fingerprint == moving_fp)
    {
        p.origin_x = best.0;
        p.origin_y = best.1;
    }
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[allow(dead_code)]
fn _device_label(d: &WorkspaceDeviceIpc) -> &str {
    d.label.as_str()
}
