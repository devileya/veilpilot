//! AppKit / CoreGraphics calls, via raw Objective-C messages.

use std::ffi::{CStr, c_void};

use objc2::encode::{Encode, Encoding};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use super::{DisplayInfo, Permission, Rect};

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGSize {
    width: f64,
    height: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

unsafe impl Encode for CGPoint {
    const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}
unsafe impl Encode for CGSize {
    const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct NSEdgeInsets {
    top: f64,
    left: f64,
    bottom: f64,
    right: f64,
}

unsafe impl Encode for NSEdgeInsets {
    const ENCODING: Encoding =
        Encoding::Struct("NSEdgeInsets", &[f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING]);
}

unsafe impl Encode for CGRect {
    const ENCODING: Encoding = Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
}

type Id = *mut AnyObject;

/// A `CGEventRef`, encoded the way AppKit's method signatures expect.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct CGEventRef(*mut c_void);

unsafe impl Encode for CGEventRef {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("__CGEvent", &[]));
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CGEventCreate(source: *const c_void) -> *mut c_void;
    fn CGEventGetLocation(event: *mut c_void) -> CGPoint;
    fn CGEventCreateScrollWheelEvent(source: *const c_void, units: u32, count: u32, wheel1: i32, ...) -> *mut c_void;
    fn CGEventSetLocation(event: *mut c_void, location: CGPoint);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

#[link(name = "AVFoundation", kind = "framework")]
unsafe extern "C" {
    static AVMediaTypeAudio: Id;
}

#[link(name = "AppKit", kind = "framework")]
unsafe extern "C" {}

// NSWindow constants.
const NS_WINDOW_SHARING_NONE: usize = 0;
const NS_WINDOW_SHARING_READ_ONLY: usize = 1;
const NS_WINDOW_STYLE_MASK_RESIZABLE: usize = 1 << 3;
const COLLECTION_CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const COLLECTION_TRANSIENT: usize = 1 << 3;
const COLLECTION_IGNORES_CYCLE: usize = 1 << 6;
const COLLECTION_FULL_SCREEN_AUXILIARY: usize = 1 << 8;
/// kCGScreenSaverWindowLevel — keeps the panel above a fullscreen Zoom/Meet
/// window, same as Electron's `setAlwaysOnTop(true, 'screen-saver', 1)`.
const SCREEN_SAVER_LEVEL: isize = 1000;
const NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY: isize = 1;

fn nsstring(text: &CStr) -> Id {
    unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

fn primary_screen_height() -> f64 {
    unsafe {
        let screens: Id = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        if count == 0 {
            return 0.0;
        }
        // The first screen is always the one with the menu bar, whose
        // bottom-left corner is the origin of AppKit's coordinate space.
        let primary: Id = msg_send![screens, objectAtIndex: 0usize];
        let frame: CGRect = msg_send![primary, frame];
        frame.size.height
    }
}

/// AppKit's bottom-left space -> global top-left points.
fn to_top_left(r: CGRect, primary_h: f64) -> Rect {
    Rect { x: r.origin.x, y: primary_h - (r.origin.y + r.size.height), width: r.size.width, height: r.size.height }
}

fn from_top_left(r: Rect, primary_h: f64) -> CGRect {
    CGRect {
        origin: CGPoint { x: r.x, y: primary_h - (r.y + r.height) },
        size: CGSize { width: r.width, height: r.height },
    }
}

pub fn displays() -> Vec<DisplayInfo> {
    let mut out = Vec::new();
    unsafe {
        let screens: Id = msg_send![class!(NSScreen), screens];
        let count: usize = msg_send![screens, count];
        let primary_h = primary_screen_height();
        let key = nsstring(c"NSScreenNumber");
        for i in 0..count {
            let screen: Id = msg_send![screens, objectAtIndex: i];
            let frame: CGRect = msg_send![screen, frame];
            let visible: CGRect = msg_send![screen, visibleFrame];
            let scale: f64 = msg_send![screen, backingScaleFactor];
            let desc: Id = msg_send![screen, deviceDescription];
            let number: Id = msg_send![desc, objectForKey: key];
            let id: u32 = if number.is_null() { i as u32 } else { msg_send![number, unsignedIntValue] };
            let bounds = to_top_left(frame, primary_h);
            out.push(DisplayInfo {
                id: id.to_string(),
                bounds,
                work_area: to_top_left(visible, primary_h),
                scale,
                primary: i == 0,
                notch: notch(screen, bounds),
            });
        }
    }
    out
}

/// The notch: the gap between the two auxiliary top areas, as tall as the
/// top safe-area inset (macOS 12+).
fn notch(screen: Id, bounds: Rect) -> Option<Rect> {
    unsafe {
        let responds: Bool = msg_send![screen, respondsToSelector: objc2::sel!(auxiliaryTopLeftArea)];
        if !responds.as_bool() {
            return None;
        }
        let insets: NSEdgeInsets = msg_send![screen, safeAreaInsets];
        if insets.top <= 0.0 {
            return None;
        }
        let left: CGRect = msg_send![screen, auxiliaryTopLeftArea];
        let right: CGRect = msg_send![screen, auxiliaryTopRightArea];
        let width = right.origin.x - (left.origin.x + left.size.width);
        if width <= 0.0 {
            return None;
        }
        Some(Rect { x: bounds.x + left.size.width, y: bounds.y, width, height: insets.top })
    }
}

pub fn cursor_position() -> (f64, f64) {
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return (0.0, 0.0);
        }
        let p = CGEventGetLocation(event);
        CFRelease(event);
        (p.x, p.y)
    }
}

/// The original `windowDidBecomeKey:` of GPUI's window classes.
static GPUI_DID_BECOME_KEY: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

unsafe extern "C-unwind" fn did_become_key(this: *mut AnyObject, sel: objc2::runtime::Sel, note: *mut AnyObject) {
    unsafe {
        let key: Bool = msg_send![&*this, isKeyWindow];
        if !key.as_bool() {
            // A spurious "became key". GPUI answers it with resignKeyWindow
            // while holding the window's lock, and the resign notification
            // that follows takes the same lock: a deadlock. Resign here,
            // with no lock held, which is what GPUI meant to do.
            let _: () = msg_send![&*this, resignKeyWindow];
            return;
        }
        if let Some(&imp) = GPUI_DID_BECOME_KEY.get() {
            let original: unsafe extern "C-unwind" fn(*mut AnyObject, objc2::runtime::Sel, *mut AnyObject) =
                std::mem::transmute(imp);
            original(this, sel, note);
        }
    }
}

/// Work around a GPUI 0.2 deadlock on spurious key-window changes (see
/// `did_become_key`). Call once, after the first window is open: GPUI
/// registers its window classes then.
pub fn guard_key_window_deadlock() {
    use objc2::ffi;
    use objc2::runtime::{AnyClass, Sel};
    let sel = Sel::register(c"windowDidBecomeKey:");
    for name in [c"GPUIWindow", c"GPUIPanel"] {
        let Some(class) = AnyClass::get(name) else { continue };
        unsafe {
            let method = ffi::class_getInstanceMethod(class, sel);
            if method.is_null() {
                continue;
            }
            let ours: objc2::runtime::Imp =
                std::mem::transmute(did_become_key as unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject));
            // Both classes share GPUI's function; keep the first original.
            if let Some(previous) = ffi::method_setImplementation(method, ours) {
                if previous as usize != ours as usize {
                    let _ = GPUI_DID_BECOME_KEY.set(previous as usize);
                }
            }
        }
    }
}

/// Accessory app: no Dock icon, no menu bar, no App Switcher entry — all of
/// which would be visible in a screen share even though the panel is not.
pub fn set_accessory_app() {
    unsafe {
        let app: Id = msg_send![class!(NSApplication), sharedApplication];
        let _: Bool = msg_send![app, setActivationPolicy: NS_APPLICATION_ACTIVATION_POLICY_ACCESSORY];
    }
}

pub fn activate_app() {
    unsafe {
        let app: Id = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: Bool::YES];
    }
}

/* ---------- permissions ---------- */

pub fn screen_permission() -> Permission {
    if unsafe { CGPreflightScreenCaptureAccess() } { Permission::Granted } else { Permission::Denied }
}

/// Adds the app to System Settings › Screen Recording (and shows the system
/// prompt the first time), so the user has something to switch on.
pub fn request_screen_permission() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

pub fn open_screen_settings() {
    super::open_external("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture");
}

pub fn microphone_permission() -> Permission {
    let status: isize =
        unsafe { msg_send![class!(AVCaptureDevice), authorizationStatusForMediaType: AVMediaTypeAudio] };
    match status {
        0 => Permission::NotDetermined,
        1 => Permission::Restricted,
        2 => Permission::Denied,
        _ => Permission::Granted,
    }
}

/// Shows the system microphone prompt; resolves with the user's answer.
pub fn request_microphone() -> futures::channel::oneshot::Receiver<bool> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let tx = std::sync::Mutex::new(Some(tx));
    let block = block2::RcBlock::new(move |granted: Bool| {
        if let Some(tx) = tx.lock().ok().and_then(|mut t| t.take()) {
            let _ = tx.send(granted.as_bool());
        }
    });
    unsafe {
        let _: () = msg_send![
            class!(AVCaptureDevice),
            requestAccessForMediaType: AVMediaTypeAudio,
            completionHandler: &*block
        ];
    }
    rx
}

/* ---------- windows ---------- */

/// The NSWindow behind a GPUI window. Only ever touched on the main thread,
/// and only while GPUI keeps the window open.
#[derive(Clone, Copy, Debug)]
pub struct NativeWindow(Id);

impl NativeWindow {
    pub fn from_window(window: &impl HasWindowHandle) -> Option<Self> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else { return None };
        let view = appkit.ns_view.as_ptr() as Id;
        let ns_window: Id = unsafe { msg_send![view, window] };
        (!ns_window.is_null()).then_some(Self(ns_window))
    }

    /// Floating HUD behaviour: above fullscreen apps, on every Space, out of
    /// Mission Control and Cmd-`, no traffic lights, natively resizable.
    pub fn configure_overlay(&self, level_offset: isize, resizable: bool) {
        unsafe {
            let w = self.0;
            let _: () = msg_send![w, setLevel: SCREEN_SAVER_LEVEL + level_offset];
            let behavior = COLLECTION_CAN_JOIN_ALL_SPACES
                | COLLECTION_FULL_SCREEN_AUXILIARY
                | COLLECTION_TRANSIENT
                | COLLECTION_IGNORES_CYCLE;
            let _: () = msg_send![w, setCollectionBehavior: behavior];
            // An NSPanel hides itself whenever the app deactivates, which for
            // an accessory app is nearly always.
            let _: () = msg_send![w, setHidesOnDeactivate: Bool::NO];
            let _: () = msg_send![w, setHasShadow: Bool::NO];
            for button in 0isize..3 {
                let b: Id = msg_send![w, standardWindowButton: button];
                if !b.is_null() {
                    let _: () = msg_send![b, setHidden: Bool::YES];
                }
            }
            let mask: usize = msg_send![w, styleMask];
            let mask =
                if resizable { mask | NS_WINDOW_STYLE_MASK_RESIZABLE } else { mask & !NS_WINDOW_STYLE_MASK_RESIZABLE };
            let _: () = msg_send![w, setStyleMask: mask];
        }
    }

    /// Drop the (invisible) title bar. AppKit keeps titled windows below the
    /// menu bar; a full-screen overlay has to cover it.
    pub fn make_borderless(&self) {
        const NON_ACTIVATING_PANEL: usize = 1 << 7;
        unsafe {
            let mask: usize = msg_send![self.0, styleMask];
            let _: () = msg_send![self.0, setStyleMask: mask & NON_ACTIVATING_PANEL];
        }
    }

    /// The point of the app. NSWindowSharingNone keeps the window out of every
    /// capture path the compositor serves: ScreenCaptureKit, CGWindowList, and
    /// therefore Zoom, Meet, Teams, OBS and screenshots.
    pub fn set_content_protection(&self, on: bool) {
        let sharing = if on { NS_WINDOW_SHARING_NONE } else { NS_WINDOW_SHARING_READ_ONLY };
        unsafe {
            let _: () = msg_send![self.0, setSharingType: sharing];
        }
    }

    pub fn set_ignore_mouse(&self, on: bool) {
        unsafe {
            let _: () = msg_send![self.0, setIgnoresMouseEvents: Bool::new(on)];
        }
    }

    pub fn set_opacity(&self, value: f32) {
        unsafe {
            let _: () = msg_send![self.0, setAlphaValue: value as f64];
        }
    }

    pub fn show(&self, focus: bool) {
        unsafe {
            if focus {
                activate_app();
                let _: () = msg_send![self.0, makeKeyAndOrderFront: std::ptr::null_mut::<AnyObject>()];
            } else {
                // Shown without focus so the meeting app keeps the keyboard.
                let _: () = msg_send![self.0, orderFrontRegardless];
            }
        }
    }

    pub fn hide(&self) {
        unsafe {
            let _: () = msg_send![self.0, orderOut: std::ptr::null_mut::<AnyObject>()];
        }
    }

    pub fn is_visible(&self) -> bool {
        let visible: Bool = unsafe { msg_send![self.0, isVisible] };
        visible.as_bool()
    }

    /// Dev: tell the window it became key when it did not, the way AppKit
    /// sometimes does (see `guard_key_window_deadlock`).
    pub fn spurious_become_key(&self) {
        let nil: Id = std::ptr::null_mut();
        unsafe {
            let _: () = msg_send![self.0, windowDidBecomeKey: nil];
        }
    }

    pub fn is_key(&self) -> bool {
        let key: Bool = unsafe { msg_send![self.0, isKeyWindow] };
        key.as_bool()
    }

    pub fn frame(&self) -> Rect {
        let frame: CGRect = unsafe { msg_send![self.0, frame] };
        to_top_left(frame, primary_screen_height())
    }

    /// The smallest size the user can drag the window to (GPUI sets it once,
    /// at open; the minimized bar needs less).
    pub fn set_min_size(&self, width: f64, height: f64) {
        unsafe {
            let _: () = msg_send![self.0, setContentMinSize: CGSize { width, height }];
        }
    }

    pub fn set_frame(&self, rect: Rect) {
        let frame = from_top_left(rect, primary_screen_height());
        unsafe {
            let _: () = msg_send![self.0, setFrame: frame, display: Bool::YES];
        }
    }

    /// Hand the current mouse-down to the window server so it drags the
    /// window natively — smooth, and correct across displays.
    pub fn begin_drag(&self) {
        unsafe {
            let app: Id = msg_send![class!(NSApplication), sharedApplication];
            let event: Id = msg_send![app, currentEvent];
            if !event.is_null() {
                let _: () = msg_send![self.0, performWindowDragWithEvent: event];
            }
        }
    }

    /// Deliver a synthetic mouse click at a point in window coordinates (top-left
    /// origin, points), through the same `sendEvent:` path a real click takes.
    pub fn send_click(&self, x: f64, y: f64, right: bool) {
        unsafe {
            let content: Id = msg_send![self.0, contentView];
            let bounds: CGRect = msg_send![content, bounds];
            let location = CGPoint { x, y: bounds.size.height - y };
            let number: isize = msg_send![self.0, windowNumber];
            let (down, up) = if right { (3usize, 4usize) } else { (1usize, 2usize) };
            // GPUI hit-tests against the last known pointer position, so the
            // pointer "moves" there first, as it would for a real click.
            for kind in [5usize, down, up] {
                let event: Id = msg_send![
                    class!(NSEvent),
                    mouseEventWithType: kind,
                    location: location,
                    modifierFlags: 0usize,
                    timestamp: 0f64,
                    windowNumber: number,
                    context: std::ptr::null_mut::<AnyObject>(),
                    eventNumber: 0isize,
                    clickCount: if kind == 5 { 0isize } else { 1isize },
                    pressure: 1f32
                ];
                if !event.is_null() {
                    let _: () = msg_send![self.0, sendEvent: event];
                }
            }
        }
    }

    /// Deliver a synthetic scroll at a point in window coordinates (dev tests).
    pub fn send_scroll(&self, x: f64, y: f64, lines: i32) {
        unsafe {
            // Scrolls go to whatever is under the last known pointer position.
            let content: Id = msg_send![self.0, contentView];
            let bounds: CGRect = msg_send![content, bounds];
            let number: isize = msg_send![self.0, windowNumber];
            let moved: Id = msg_send![
                class!(NSEvent),
                mouseEventWithType: 5usize,
                location: CGPoint { x, y: bounds.size.height - y },
                modifierFlags: 0usize,
                timestamp: 0f64,
                windowNumber: number,
                context: std::ptr::null_mut::<AnyObject>(),
                eventNumber: 0isize,
                clickCount: 0isize,
                pressure: 0f32
            ];
            if !moved.is_null() {
                let _: () = msg_send![self.0, sendEvent: moved];
            }
            let frame = self.frame();
            let cg = CGEventCreateScrollWheelEvent(std::ptr::null(), 1, 1, lines);
            if cg.is_null() {
                return;
            }
            CGEventSetLocation(cg, CGPoint { x: frame.x + x, y: frame.y + y });
            let event: Id = msg_send![class!(NSEvent), eventWithCGEvent: CGEventRef(cg)];
            if !event.is_null() {
                let _: () = msg_send![self.0, sendEvent: event];
            }
            CFRelease(cg);
        }
    }

    /// The CGWindowID, for capturing our own window in self-tests.
    pub fn window_number(&self) -> isize {
        unsafe { msg_send![self.0, windowNumber] }
    }
}
