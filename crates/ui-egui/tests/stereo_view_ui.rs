//! View ▸ Stereo View, driven headless with a recording host in place of the native window: the
//! command is offered for a stereo document on a host that has a window; it hands the host the
//! canvas picture as one stereo pair (full side by side), follows edits, sends nothing while it
//! is off, and follows the window being closed. The picture of a document that is not a pair is
//! passed as it is, marked as not stereo (the window never declares it).

use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::Document;
use photocraft_engine::Session;
use photocraft_geom::Size;
use photocraft_ui_egui::stereo_view::{StereoFrame, StereoViewHost};
use photocraft_ui_egui::{PhotocraftApp, Services, menus};
use serde_json::{Value, json};

#[derive(Default)]
struct Log {
    /// (width, height, buffer length, stereo, first pixel) of every picture shown
    shown: Vec<(usize, usize, usize, bool, [u8; 4])>,
    open: bool,
    closes: usize,
}

struct Recorder(Arc<Mutex<Log>>);

impl StereoViewHost for Recorder {
    fn show(&mut self, frame: StereoFrame) -> Result<(), String> {
        let mut log = self.0.lock().unwrap();
        let first = [frame.rgba[0], frame.rgba[1], frame.rgba[2], frame.rgba[3]];
        log.shown.push((frame.width, frame.height, frame.rgba.len(), frame.stereo, first));
        log.open = true;
        Ok(())
    }
    fn is_open(&self) -> bool {
        self.0.lock().unwrap().open
    }
    fn close(&mut self) {
        let mut log = self.0.lock().unwrap();
        log.open = false;
        log.closes += 1;
    }
}

struct Driver {
    harness: Harness<'static, PhotocraftApp>,
    log: Arc<Mutex<Log>>,
}

impl Driver {
    fn new(with_window: bool) -> Self {
        let log = Arc::new(Mutex::new(Log::default()));
        let services = Services { stereo_view: with_window.then(|| Box::new(Recorder(log.clone())) as Box<dyn StereoViewHost>), ..Default::default() };
        let app = PhotocraftApp::new(Session::new(), services);
        let harness = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_max_steps(1000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, log };
        d.frames(3);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.harness.step();
        }
    }
    fn app(&mut self) -> &mut PhotocraftApp {
        self.harness.state_mut()
    }
    /// A document of `w` x `h`, a stereo pair or not, filled with a colour.
    fn open(&mut self, name: &str, w: u32, h: u32, stereo: bool, colour: &str) {
        let mut doc = Document::new(name, Size::new(w, h), ColorMode::Rgb, SampleType::U8);
        doc.stereo = stereo;
        self.app().session.add_document(doc, None);
        self.app().run("layer.new.layer", json!({})).unwrap();
        self.app().run("edit.fill", json!({"color": colour})).unwrap();
        self.frames(3);
    }
    fn invoke(&mut self, id: &str) -> Result<Value, String> {
        let ctx = self.harness.ctx.clone();
        let r = menus::invoke(self.harness.state_mut(), &ctx, id, Value::Null);
        self.frames(3);
        r
    }
    fn item(&mut self) -> (bool, Option<bool>) {
        let items = menus::menu_items(self.harness.state());
        let m = items.iter().find(|i| i.id == "view.stereoView").expect("View > Stereo View");
        assert_eq!(m.path, vec!["View".to_string()]);
        (m.enabled, m.checked)
    }
    fn shown(&self) -> usize {
        self.log.lock().unwrap().shown.len()
    }
}

#[test]
fn the_stereo_view_is_offered_for_a_stereo_document_only() {
    let mut d = Driver::new(true);
    assert!(!d.item().0, "no document");
    d.open("Flat", 64, 36, false, "#808080");
    assert!(!d.item().0, "a flat document has no pair to show");
    assert!(d.invoke("view.stereoView").is_err());
    d.frames(10);
    assert_eq!(d.shown(), 0, "nothing is shown while it is off");

    d.open("Pair", 128, 36, true, "#336699");
    assert_eq!(d.item(), (true, Some(false)));
}

#[test]
fn a_host_without_a_window_leaves_the_command_off() {
    let mut d = Driver::new(false);
    d.open("Pair", 128, 36, true, "#336699");
    assert!(!d.item().0);
    assert!(d.invoke("view.stereoView").is_err());
}

#[test]
fn the_window_gets_the_canvas_as_one_stereo_picture_while_it_is_on() {
    let mut d = Driver::new(true);
    d.open("Pair", 128, 36, true, "#336699");
    d.invoke("view.stereoView").unwrap();
    assert_eq!(d.item(), (true, Some(true)));
    assert_eq!(d.shown(), 1);
    // the document's own size: full side by side, 64 x 36 per eye, RGBA, as the canvas shows it
    assert_eq!(d.log.lock().unwrap().shown[0], (128, 36, 128 * 36 * 4, true, [0x33, 0x66, 0x99, 0xff]));

    // an edit reaches the window; an unchanged canvas sends nothing
    d.frames(20);
    assert_eq!(d.shown(), 1);
    d.app().run("edit.fill", json!({"color": "#ff0000"})).unwrap();
    d.frames(3);
    assert_eq!(d.shown(), 2);
    assert_eq!(d.log.lock().unwrap().shown[1].4, [0xff, 0, 0, 0xff]);

    // off: the host closes the window and gets nothing more
    d.invoke("view.stereoView").unwrap();
    assert_eq!(d.log.lock().unwrap().closes, 1);
    d.app().run("edit.fill", json!({"color": "#00ff00"})).unwrap();
    d.frames(30);
    assert_eq!(d.shown(), 2, "nothing is shown while it is off");
}

#[test]
fn closing_the_window_turns_the_command_off() {
    let mut d = Driver::new(true);
    d.open("Pair", 128, 36, true, "#336699");
    d.invoke("view.stereoView").unwrap();
    // the user closes the window
    d.log.lock().unwrap().open = false;
    d.frames(6);
    assert_eq!(d.item().1, Some(false));
    let n = d.shown();
    d.frames(30);
    assert_eq!(d.shown(), n, "a closed window gets no pictures");
}

#[test]
fn a_document_that_is_not_a_pair_is_passed_as_not_stereo() {
    let mut d = Driver::new(true);
    d.open("Pair", 128, 36, true, "#336699");
    d.invoke("view.stereoView").unwrap();
    assert!(d.log.lock().unwrap().shown[0].3);
    // a flat document becomes the active one; the window follows it and the picture says 2D,
    // which is what keeps the window from declaring it
    d.open("Flat", 128, 36, false, "#808080");
    let last = *d.log.lock().unwrap().shown.last().unwrap();
    assert_eq!(last, (128, 36, 128 * 36 * 4, false, [0x80, 0x80, 0x80, 0xff]));
}

#[test]
fn closing_the_last_document_turns_the_command_off() {
    let mut d = Driver::new(true);
    d.open("Pair", 128, 36, true, "#336699");
    d.invoke("view.stereoView").unwrap();
    d.app().run("file.close", json!({"discard": true})).unwrap();
    d.frames(6);
    assert_eq!(d.item(), (false, Some(false)));
}
