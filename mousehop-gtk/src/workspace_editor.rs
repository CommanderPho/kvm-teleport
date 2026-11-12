//! Minimal shared-workspace layout editor (GTK).
//!
//! Authority edits device-group origins and Applies; followers are read-only.
//! Daemon owns validation, persistence, and peer sync.

use adw::prelude::*;
use gtk::prelude::*;
use gtk::{Orientation, SpinButton};
use mousehop_ipc::{FrontendRequest, WorkspaceIpc, WorkspacePlacementIpc};
use std::cell::RefCell;
use std::rc::Rc;

pub fn present_workspace_editor(
    parent: &impl IsA<gtk::Window>,
    request: Rc<dyn Fn(FrontendRequest)>,
    initial: WorkspaceIpc,
) {
    let dialog = adw::Window::new();
    dialog.set_transient_for(Some(parent));
    dialog.set_modal(true);
    dialog.set_title(Some("Shared layout"));
    dialog.set_default_size(560, 480);

    let root = gtk::Box::new(Orientation::Vertical, 10);
    root.set_margin_top(16);
    root.set_margin_bottom(16);
    root.set_margin_start(16);
    root.set_margin_end(16);

    let banner = gtk::Label::new(Some(if initial.is_authority {
        "You are the workspace authority. Adjust device origins (logical pixels), then Apply."
    } else {
        "Follower (read-only). The authority publishes the shared layout."
    }));
    banner.set_wrap(true);
    root.append(&banner);

    let meta = gtk::Label::new(Some(&format!(
        "workspace={}  revision={}  authority={}",
        if initial.workspace_id.is_empty() {
            "(none)"
        } else {
            &initial.workspace_id
        },
        initial.revision,
        trunc(&initial.authority_fingerprint, 12)
    )));
    meta.add_css_class("dim-label");
    root.append(&meta);

    let state = Rc::new(RefCell::new(initial.clone()));
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");

    for placement in &initial.placements {
        let row = adw::ActionRow::new();
        row.set_title(&trunc(&placement.fingerprint, 16));
        let presence = initial
            .devices
            .iter()
            .find(|d| d.fingerprint == placement.fingerprint)
            .map(|d| d.presence.as_str())
            .unwrap_or("remote");
        row.set_subtitle(presence);

        let x = SpinButton::with_range(-20000.0, 20000.0, 10.0);
        x.set_value(f64::from(placement.origin_x));
        x.set_digits(0);
        x.set_sensitive(initial.is_authority);
        let y = SpinButton::with_range(-20000.0, 20000.0, 10.0);
        y.set_value(f64::from(placement.origin_y));
        y.set_digits(0);
        y.set_sensitive(initial.is_authority);

        let fp = placement.fingerprint.clone();
        x.connect_value_changed(glib::clone!(
            #[strong]
            state,
            #[strong]
            fp,
            move |spin| {
                if let Some(p) = state
                    .borrow_mut()
                    .placements
                    .iter_mut()
                    .find(|p| p.fingerprint == fp)
                {
                    p.origin_x = spin.value() as i32;
                }
            }
        ));
        let fp_y = placement.fingerprint.clone();
        y.connect_value_changed(glib::clone!(
            #[strong]
            state,
            move |spin| {
                if let Some(p) = state
                    .borrow_mut()
                    .placements
                    .iter_mut()
                    .find(|p| p.fingerprint == fp_y)
                {
                    p.origin_y = spin.value() as i32;
                }
            }
        ));

        let coords = gtk::Box::new(Orientation::Horizontal, 6);
        coords.append(&gtk::Label::new(Some("X")));
        coords.append(&x);
        coords.append(&gtk::Label::new(Some("Y")));
        coords.append(&y);
        row.add_suffix(&coords);
        list.append(&row);
    }
    root.append(&list);

    let routes = gtk::Label::new(Some(&format_routes(&initial)));
    routes.set_wrap(true);
    routes.set_xalign(0.0);
    routes.add_css_class("dim-label");
    root.append(&routes);

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

    if initial.is_authority {
        let apply = gtk::Button::with_label("Apply");
        apply.add_css_class("suggested-action");
        apply.connect_clicked(glib::clone!(
            #[strong]
            state,
            #[strong]
            dialog,
            #[strong]
            request,
            move |_| {
                let mut ws = state.borrow().clone();
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
                request(FrontendRequest::ApplyWorkspace(ws));
                dialog.close();
            }
        ));
        buttons.append(&apply);
    }

    root.append(&buttons);
    dialog.set_content(Some(&root));
    dialog.present();
}

fn trunc(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn format_routes(ws: &WorkspaceIpc) -> String {
    if ws.routes.is_empty() {
        return "No traversable edges yet (place devices so monitor edges touch).".into();
    }
    let mut lines = vec!["Traversable crossings:".to_string()];
    for r in &ws.routes {
        lines.push(format!(
            "• {} {} [{}..{}) → {}",
            trunc(&r.src_fingerprint, 8),
            r.edge,
            r.start,
            r.end,
            trunc(&r.dst_fingerprint, 8)
        ));
    }
    lines.join("\n")
}
