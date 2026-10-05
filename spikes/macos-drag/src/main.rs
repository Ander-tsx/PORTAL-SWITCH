//! Spike: file drag & drop between machines — macOS side.
//!
//! Modes (logs to ~/Library/Logs/portalswitch-spike/drag.log and stdout):
//!   detect <secs>          watch for a file drag on this Mac: left button held + drag pasteboard
//!                          change; prints detection delay and the dragged items. No permissions.
//!   drop-window <secs>     shows a small blue window; drag it onto Finder/Desktop. A file promise
//!                          tells us the destination folder, and we write a test file there.
//!   drop-synthetic <x> <y> destination flow of a KVM: a tiny transparent window under the cursor,
//!                          synthetic mouse-down on it starts a native drag carrying a file promise,
//!                          synthetic drag to (x, y) (global, top-left origin) and mouse-up. Needs
//!                          Accessibility (run as the signed bundle via run.sh).
//!   catch <secs>           source-side alternative: a transparent strip on the right screen edge
//!                          registered as a drag destination; any standard drag that reaches the
//!                          edge is caught and its items logged. Drops on the strip are accepted.

use std::io::Write as _;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use block2::DynBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSColor, NSDragOperation,
    NSDraggingContext, NSDraggingDestination, NSDraggingInfo, NSDraggingItem, NSDraggingSession,
    NSDraggingSource, NSEvent, NSFilePromiseProvider, NSFilePromiseProviderDelegate, NSPasteboard,
    NSPasteboardNameDrag, NSPasteboardTypeFileURL, NSResponder, NSScreen, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{NSArray, NSError, NSPoint, NSRect, NSSize, NSString, NSURL, ns_string};

// ---------------------------------------------------------------------------------------------
// CoreGraphics FFI (events)
// ---------------------------------------------------------------------------------------------

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct CGPoint {
    x: f64,
    y: f64,
}

type CGEventRef = *mut std::ffi::c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceButtonState(state: i32, button: u32) -> bool;
    fn CGEventCreate(source: *mut std::ffi::c_void) -> CGEventRef;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGEventCreateMouseEvent(
        source: *mut std::ffi::c_void,
        kind: u32,
        location: CGPoint,
        button: u32,
    ) -> CGEventRef;
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CFRelease(cf: *const std::ffi::c_void);
    fn AXIsProcessTrusted() -> bool;
}

const HID_STATE: i32 = 1;
const LEFT: u32 = 0;
const EV_LEFT_DOWN: u32 = 1;
const EV_LEFT_UP: u32 = 2;
const EV_LEFT_DRAGGED: u32 = 6;
const HID_TAP: u32 = 0;

fn cursor() -> CGPoint {
    // SAFETY: +1 event released after reading.
    unsafe {
        let e = CGEventCreate(std::ptr::null_mut());
        let p = CGEventGetLocation(e);
        CFRelease(e.cast());
        p
    }
}

fn post_mouse(kind: u32, p: CGPoint) {
    // SAFETY: valid event; released after posting.
    unsafe {
        let e = CGEventCreateMouseEvent(std::ptr::null_mut(), kind, p, LEFT);
        CGEventPost(HID_TAP, e);
        CFRelease(e.cast());
    }
}

// ---------------------------------------------------------------------------------------------
// Logging / timing
// ---------------------------------------------------------------------------------------------

static T0: OnceLock<Instant> = OnceLock::new();

fn ms() -> f64 {
    T0.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

fn log(msg: &str) {
    let line = format!("[{:9.1} ms] {msg}", ms());
    println!("{line}");
    let dir = format!(
        "{}/Library/Logs/portalswitch-spike",
        std::env::var("HOME").unwrap_or_default()
    );
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(format!("{dir}/drag.log"))
    {
        let _ = writeln!(f, "{line}");
    }
}

// ---------------------------------------------------------------------------------------------
// detect: drag source detection
// ---------------------------------------------------------------------------------------------

fn dragged_items(pb: &NSPasteboard) -> Vec<String> {
    let Some(items) = pb.pasteboardItems() else {
        return vec![];
    };
    let mut out = vec![];
    for item in &items {
        // SAFETY: static pasteboard type constant.
        let Some(s) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else {
            continue;
        };
        let path = NSURL::URLWithString(&s)
            .and_then(|u| u.filePathURL())
            .and_then(|u| u.path())
            .map_or_else(|| s.to_string(), |p| p.to_string());
        let info = match std::fs::metadata(&path) {
            Ok(m) if m.is_dir() => "dir".to_string(),
            Ok(m) => format!("file {} bytes", m.len()),
            Err(e) => format!("unreadable: {e}"),
        };
        let icloud = if path.ends_with(".icloud") {
            " (iCloud placeholder)"
        } else {
            ""
        };
        out.push(format!("{path} [{info}]{icloud}"));
    }
    out
}

fn detect(secs: u64) {
    // SAFETY: static pasteboard name constant.
    let pb = NSPasteboard::pasteboardWithName(unsafe { NSPasteboardNameDrag });
    log(&format!(
        "detect: watching for {secs} s; drag files in Finder or other apps"
    ));
    let end = Instant::now() + Duration::from_secs(secs);
    let mut down: Option<(Instant, isize)> = None;
    let mut reported = false;
    while Instant::now() < end {
        // SAFETY: no preconditions.
        let pressed = unsafe { CGEventSourceButtonState(HID_STATE, LEFT) };
        let cc = pb.changeCount();
        match (pressed, down) {
            (true, None) => {
                down = Some((Instant::now(), cc));
                reported = false;
            }
            (true, Some((t, cc0))) if !reported && cc != cc0 => {
                reported = true;
                let items = dragged_items(&pb);
                log(&format!(
                    "drag detected {:.1} ms after button-down; {} item(s)",
                    t.elapsed().as_secs_f64() * 1000.0,
                    items.len()
                ));
                for i in items {
                    log(&format!("  {i}"));
                }
            }
            (false, Some((t, _))) => {
                if reported {
                    log(&format!(
                        "drag ended after {:.0} ms (button up)",
                        t.elapsed().as_secs_f64() * 1000.0
                    ));
                }
                down = None;
            }
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    log("detect: done");
}

// ---------------------------------------------------------------------------------------------
// drop: file promise drag source
// ---------------------------------------------------------------------------------------------

static MOUSE_UP_MS: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicBool = AtomicBool::new(false);

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "PSPromiseDelegate"]
    struct PromiseDelegate;

    unsafe impl NSObjectProtocol for PromiseDelegate {}

    unsafe impl NSFilePromiseProviderDelegate for PromiseDelegate {
        #[unsafe(method_id(filePromiseProvider:fileNameForType:))]
        fn file_name(
            &self,
            _provider: &NSFilePromiseProvider,
            _ty: &NSString,
        ) -> Retained<NSString> {
            log("promise: fileNameForType asked");
            NSString::from_str("PortalSwitch drop test.txt")
        }

        #[unsafe(method(filePromiseProvider:writePromiseToURL:completionHandler:))]
        fn write(
            &self,
            _provider: &NSFilePromiseProvider,
            url: &NSURL,
            completion: &DynBlock<dyn Fn(*mut NSError)>,
        ) {
            let path = url.path().map_or_else(|| "?".into(), |p| p.to_string());
            let up = MOUSE_UP_MS.load(Ordering::SeqCst);
            let since_up = if up > 0 {
                format!("{:.1} ms after mouse-up", ms() - up as f64 / 1000.0)
            } else {
                "mouse-up time unknown".into()
            };
            log(&format!(
                "promise: write to {path} ({since_up}); thread main={}",
                MainThreadMarker::new().is_some()
            ));
            let result = std::fs::write(&path, b"Dropped by the PortalSwitch drag spike.\n");
            log(&format!("promise: wrote file: {result:?}"));
            completion.call((std::ptr::null_mut(),));
            DONE.store(true, Ordering::SeqCst);
        }
    }
);

impl PromiseDelegate {
    fn new() -> Retained<Self> {
        // SAFETY: plain NSObject subclass without ivars.
        unsafe { msg_send![Self::alloc(), init] }
    }
}

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PSDragSourceView"]
    struct SourceView;

    impl SourceView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            log("view: mouseDown received, starting drag session");
            self.start_drag(event);
        }
    }

    unsafe impl NSObjectProtocol for SourceView {}

    unsafe impl NSDraggingSource for SourceView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, point: NSPoint, operation: NSDragOperation) {
            log(&format!(
                "session: ended at ({:.0}, {:.0}) operation={:?} (None = no drop target accepted)",
                point.x, point.y, operation
            ));
            if operation == NSDragOperation::None {
                DONE.store(true, Ordering::SeqCst);
            }
        }
    }
);

impl SourceView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        // SAFETY: NSView designated initializer.
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] }
    }

    fn start_drag(&self, event: &NSEvent) {
        let delegate = PromiseDelegate::new();
        let provider = NSFilePromiseProvider::initWithFileType_delegate(
            NSFilePromiseProvider::alloc(),
            ns_string!("public.plain-text"),
            ProtocolObject::from_ref(&*delegate),
        );
        // The provider holds its delegate weakly; keep it alive for the whole spike.
        std::mem::forget(delegate);
        let item = NSDraggingItem::initWithPasteboardWriter(
            NSDraggingItem::alloc(),
            ProtocolObject::from_ref(&*provider),
        );
        // SAFETY: frame in view coordinates; no custom drag image.
        unsafe { item.setDraggingFrame_contents(self.bounds(), None) };
        let items = NSArray::from_retained_slice(&[item]);
        let _session = self.beginDraggingSessionWithItems_event_source(
            &items,
            event,
            ProtocolObject::from_ref(self),
        );
        log("view: drag session started");
    }
}

// ---------------------------------------------------------------------------------------------
// catch: drag destination strip at the screen edge
// ---------------------------------------------------------------------------------------------

static ENTERED_MS: AtomicU64 = AtomicU64::new(0);

define_class!(
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "PSCatchView"]
    struct CatchView;

    unsafe impl NSObjectProtocol for CatchView {}

    unsafe impl NSDraggingDestination for CatchView {
        #[unsafe(method(draggingEntered:))]
        fn entered(&self, sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            ENTERED_MS.store((ms() * 1000.0) as u64, Ordering::SeqCst);
            let items = dragged_items(&sender.draggingPasteboard());
            let loc = sender.draggingLocation();
            log(&format!(
                "catch: drag entered edge at ({:.0}, {:.0}); source allows {:?}; {} file item(s)",
                loc.x,
                loc.y,
                sender.draggingSourceOperationMask(),
                items.len()
            ));
            for i in items {
                log(&format!("  {i}"));
            }
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingUpdated:))]
        fn updated(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(draggingExited:))]
        fn exited(&self, _sender: Option<&ProtocolObject<dyn NSDraggingInfo>>) {
            let held = ms() - ENTERED_MS.load(Ordering::SeqCst) as f64 / 1000.0;
            log(&format!("catch: drag left the edge after {held:.0} ms"));
        }

        #[unsafe(method(performDragOperation:))]
        fn perform(&self, _sender: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            log("catch: drop on the edge accepted (a KVM would deliver to the other machine here)");
            true
        }
    }
);

impl CatchView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        // SAFETY: NSView designated initializer.
        let view: Retained<Self> = unsafe { msg_send![Self::alloc(mtm), initWithFrame: frame] };
        // SAFETY: static pasteboard type constant.
        let types = NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]);
        view.registerForDraggedTypes(&types);
        view
    }
}

fn run_catch(mtm: MainThreadMarker, secs: u64) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let screen = NSScreen::mainScreen(mtm).expect("a main screen").frame();
    let width = 6.0;
    let frame = NSRect::new(
        NSPoint::new(screen.origin.x + screen.size.width - width, screen.origin.y),
        NSSize::new(width, screen.size.height),
    );
    // SAFETY: standard window creation on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by our Retained.
    unsafe { window.setReleasedWhenClosed(false) };
    let view = CatchView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), frame.size));
    window.setContentView(Some(&view));
    window.setOpaque(false);
    // Nearly transparent but not fully: fully transparent pixels may not be hit-tested.
    window.setBackgroundColor(Some(&NSColor::colorWithWhite_alpha(0.5, 0.02)));
    window.setIgnoresMouseEvents(false);
    window.setLevel(25); // status-bar level, above normal windows
    window.orderFrontRegardless();
    log(&format!(
        "catch: edge strip ready at x={:.0}..{:.0}; drag files to the right edge for {secs} s",
        frame.origin.x,
        frame.origin.x + width
    ));
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        log("catch: done");
        std::process::exit(0);
    });
    app.run();
}

fn make_window(mtm: MainThreadMarker, visible: bool) -> Retained<NSWindow> {
    let size = if visible {
        NSSize::new(160.0, 80.0)
    } else {
        NSSize::new(40.0, 40.0)
    };
    let mouse = NSEvent::mouseLocation();
    let frame = NSRect::new(
        NSPoint::new(mouse.x - size.width / 2.0, mouse.y - size.height / 2.0),
        size,
    );
    let style = if visible {
        NSWindowStyleMask::Titled
    } else {
        NSWindowStyleMask::Borderless
    };
    // SAFETY: standard window creation on the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by our Retained, not released on close.
    unsafe { window.setReleasedWhenClosed(false) };
    let view = SourceView::new(mtm, NSRect::new(NSPoint::new(0.0, 0.0), size));
    window.setContentView(Some(&view));
    if visible {
        window.setTitle(ns_string!("Drag me"));
        window.setBackgroundColor(Some(&NSColor::systemBlueColor()));
    } else {
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setIgnoresMouseEvents(false);
    }
    window.setLevel(3); // floating
    window.orderFrontRegardless();
    window
}

fn run_app(mtm: MainThreadMarker, visible: bool, timeout: Duration, driver: Option<CGPoint>) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let _window = make_window(mtm, visible);
    app.activate();
    log(&format!(
        "window shown at cursor {:?}; accessibility={}",
        cursor(),
        unsafe { AXIsProcessTrusted() }
    ));

    if let Some(target) = driver {
        std::thread::spawn(move || drive_synthetic_drag(target));
    }
    std::thread::spawn(move || {
        let end = Instant::now() + timeout;
        while Instant::now() < end && !DONE.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
        std::thread::sleep(Duration::from_millis(500));
        log(if DONE.load(Ordering::SeqCst) {
            "done"
        } else {
            "timeout"
        });
        std::process::exit(0);
    });
    app.run();
}

/// What the receiving machine of a KVM does: press on our window, drag to the target, release.
fn drive_synthetic_drag(target: CGPoint) {
    std::thread::sleep(Duration::from_millis(600));
    let start = cursor();
    log(&format!("synthetic: mouse-down at {start:?}"));
    post_mouse(EV_LEFT_DOWN, start);
    std::thread::sleep(Duration::from_millis(150));
    let steps = 40;
    for i in 1..=steps {
        let f = f64::from(i) / f64::from(steps);
        let p = CGPoint {
            x: start.x + (target.x - start.x) * f,
            y: start.y + (target.y - start.y) * f,
        };
        post_mouse(EV_LEFT_DRAGGED, p);
        std::thread::sleep(Duration::from_millis(16));
    }
    std::thread::sleep(Duration::from_millis(300));
    MOUSE_UP_MS.store((ms() * 1000.0) as u64, Ordering::SeqCst);
    log(&format!("synthetic: mouse-up at {target:?}"));
    post_mouse(EV_LEFT_UP, target);
}

fn main() {
    T0.get_or_init(Instant::now);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize, d: f64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let mtm = MainThreadMarker::new().expect("must run on the main thread");
    match args.first().map(String::as_str) {
        Some("detect") => detect(num(1, 30.0) as u64),
        Some("drop-window") => run_app(mtm, true, Duration::from_secs(num(1, 60.0) as u64), None),
        Some("catch") => run_catch(mtm, num(1, 60.0) as u64),
        Some("drop-synthetic") => {
            let target = CGPoint {
                x: num(1, 200.0),
                y: num(2, 200.0),
            };
            run_app(mtm, false, Duration::from_secs(15), Some(target));
        }
        _ => eprintln!(
            "usage: macos-drag detect <secs> | catch <secs> | drop-window <secs> | drop-synthetic <x> <y>"
        ),
    }
}
