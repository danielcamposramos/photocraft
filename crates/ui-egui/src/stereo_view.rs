//! View ▸ Stereo View: the open stereo document in a window of its own, declared to the desktop.
//!
//! A stereo document is one picture holding a pair, full side by side with the left eye in the
//! left half (`Document::stereo`). The canvas shows it as it is, both eyes in a wide picture. The
//! Stereo View hands the same picture, as the canvas shows it, to the host's stereo window, which
//! tells the desktop it holds a stereo pair (the desktop then shows it on each screen as that
//! screen can: a 3D mode, or the left eye on a plain screen). The program has no output formats
//! of its own: one format goes out, declared once, and only while a stereo picture is shown.
//!
//! The window belongs to the host ([`StereoViewHost`], `Services::stereo_view`): the desktop app
//! implements it with `photocraft-stereo-window`; hosts without one (the web) leave the command
//! disabled.

use std::sync::Arc;

use photocraft_doc::DocId;

use crate::PhotocraftApp;

/// One picture for the host's stereo window: RGBA8, straight alpha. A stereo picture is full
/// side by side with the left eye first.
pub struct StereoFrame {
    pub width: usize,
    pub height: usize,
    pub rgba: Arc<[u8]>,
    /// The picture holds a stereo pair.
    pub stereo: bool,
}

/// A window the host shows the stereo picture in.
pub trait StereoViewHost {
    /// Shows the next picture, opening the window with the first one. An error says why it could
    /// not be opened.
    fn show(&mut self, frame: StereoFrame) -> Result<(), String>;
    /// The window is on screen (false once the user closed it).
    fn is_open(&self) -> bool;
    /// Closes the window.
    fn close(&mut self);
}

/// The Stereo View's state.
#[derive(Default)]
pub struct StereoView {
    /// View ▸ Stereo View is on.
    pub wanted: bool,
    /// The host has the window open.
    opened: bool,
    /// The picture the window shows: the document and the key of what the canvas shows of it.
    shown: Option<(DocId, u64)>,
}

/// Whether the active document is a stereo pair.
pub fn shows_stereo(app: &PhotocraftApp) -> bool {
    app.session.active().is_some_and(|d| d.doc.stereo)
}

/// Menu enablement of `view.stereoView`: a host with a window and a stereo document to show (or
/// the window already on, so it can be turned off).
pub fn enabled(app: &PhotocraftApp) -> bool {
    app.stereo_view.wanted || (app.services.stereo_view.is_some() && shows_stereo(app))
}

/// `view.stereoView`: turns the window on or off.
pub fn toggle(app: &mut PhotocraftApp, on: bool) -> Result<(), String> {
    if on && !app.stereo_view.wanted && !enabled(app) {
        return Err(if app.services.stereo_view.is_none() {
            "this host has no stereo window".into()
        } else {
            "the active document is not a stereo pair".into()
        });
    }
    app.stereo_view.wanted = on;
    Ok(())
}

/// Once per UI pass: opens, feeds and closes the window.
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(mut host) = app.services.stereo_view.take() else {
        app.stereo_view.wanted = false;
        return;
    };
    step(app, ctx, host.as_mut());
    app.services.stereo_view = Some(host);
}

fn step(app: &mut PhotocraftApp, ctx: &egui::Context, host: &mut dyn StereoViewHost) {
    if !app.stereo_view.wanted {
        if std::mem::take(&mut app.stereo_view.opened) {
            host.close();
        }
        app.stereo_view.shown = None;
        return;
    }
    if app.stereo_view.opened && !host.is_open() {
        // the user closed the window: the menu entry goes off with it
        app.stereo_view.wanted = false;
        app.stereo_view.opened = false;
        app.stereo_view.shown = None;
        return;
    }
    let Some(idx) = app.session.active_index() else {
        // no document left to show
        app.stereo_view.wanted = false;
        return;
    };
    let (id, key, stereo, render) = crate::canvas::shown_rgba8(app, idx);
    if app.stereo_view.shown == Some((id, key)) {
        return;
    }
    let img = render();
    let picture = StereoFrame { width: img.width as usize, height: img.height as usize, rgba: Arc::from(img.pixels), stereo };
    match host.show(picture) {
        Ok(()) => {
            app.stereo_view.opened = true;
            app.stereo_view.shown = Some((id, key));
        }
        Err(e) => {
            app.stereo_view.wanted = false;
            app.stereo_view.opened = host.is_open();
            crate::notices::error(app, format!("Stereo View: {e}"));
            ctx.request_repaint();
        }
    }
}
