//! The Accessibility API, as much of it as Minion needs.
//!
//! Everything else Minion does to another application it does through a
//! keystroke or through AppleScript. Both stop at the window: neither can
//! say "which of these is a text field", "what are this application's
//! windows called" or "put the focus there". The Accessibility API can,
//! and Minion already holds the permission it needs — it is what
//! `actions::press` depends on.
//!
//! Written against the C API directly rather than through a crate: the
//! whole of what is used here is five functions, and a hand-written
//! `extern` block is smaller than a dependency, easier to read, and
//! cannot drift with somebody else's release schedule. Same bargain
//! `microphone.rs` makes with CoreAudio.
//!
//! Two rules hold this file together:
//!
//!   * an [`Element`] owns its reference and releases it once, so the
//!     Core Foundation create/get rules are obeyed in one place rather
//!     than at every call site;
//!   * nothing here talks to the Accessibility API under `cfg!(test)`.
//!     An element can only be born in [`application`], [`frontmost`] or
//!     [`running_apps`], and all three refuse in a test, so a test run
//!     cannot reach into whatever windows happen to be open on the
//!     machine it runs on.

use core_foundation::array::{CFArrayGetCount, CFArrayGetTypeID, CFArrayGetValueAtIndex, CFArrayRef};
use core_foundation::base::{CFGetTypeID, CFRelease, CFRetain, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};
use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};

/// Attribute and action names, spelled as the framework spells them.
pub const ROLE: &str = "AXRole";
pub const TITLE: &str = "AXTitle";
pub const WINDOWS: &str = "AXWindows";
pub const CHILDREN: &str = "AXChildren";
pub const FOCUSED: &str = "AXFocused";
pub const FOCUSED_UI_ELEMENT: &str = "AXFocusedUIElement";
pub const RAISE: &str = "AXRaise";

/// The roles that can be typed into. Deliberately short: a combo box or a
/// web area may or may not take text, and focusing one that does not is
/// worse than leaving the focus where it was.
pub const TEXT_ROLES: &[&str] = &["AXTextField", "AXTextArea"];

type AXUIElementRef = CFTypeRef;
type AXError = i32;
const SUCCESS: AXError = 0;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
}

/// Whether this process may use the Accessibility API at all.
///
/// Asked of `actions::has_accessibility_permission`, which is the same
/// question `press` already depends on — and never the prompting variant
/// of it, since `main.rs` owns when System Settings is allowed to open
/// (once per boot, not once per launch).
pub fn trusted() -> bool {
    if cfg!(test) {
        return false;
    }
    crate::actions::has_accessibility_permission()
}

/// One accessibility element, owning its reference.
pub struct Element(AXUIElementRef);

impl Drop for Element {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

impl Element {
    /// Takes ownership of a reference that arrived with a +1 count, as
    /// everything named `Create` or `Copy` does.
    ///
    /// # Safety
    /// `raw` must be a reference the caller owns and does not release.
    unsafe fn owning(raw: CFTypeRef) -> Option<Self> {
        (!raw.is_null()).then_some(Element(raw))
    }

    /// The raw value of an attribute, +1, or `None` when it has none.
    fn attribute(&self, name: &str) -> Option<CFTypeRef> {
        let key = CFString::new(name);
        let mut value: CFTypeRef = std::ptr::null();
        let status =
            unsafe { AXUIElementCopyAttributeValue(self.0, key.as_concrete_TypeRef(), &mut value) };
        (status == SUCCESS && !value.is_null()).then_some(value)
    }

    /// An attribute that is a string: `AXRole`, `AXTitle`.
    pub fn text(&self, name: &str) -> Option<String> {
        let value = self.attribute(name)?;
        unsafe {
            if CFGetTypeID(value) != CFString::type_id() {
                CFRelease(value);
                return None;
            }
            Some(CFString::wrap_under_create_rule(value as CFStringRef).to_string())
        }
    }

    /// An attribute that is another element: `AXFocusedUIElement`.
    pub fn element(&self, name: &str) -> Option<Element> {
        let value = self.attribute(name)?;
        unsafe { Element::owning(value) }
    }

    /// An attribute that is a list of elements: `AXWindows`, `AXChildren`.
    ///
    /// Empty when there is no such attribute, when it is not a list, or
    /// when the application refuses to answer — an unresponsive
    /// application is not an error worth a message, it is an application
    /// with no windows as far as anything here is concerned.
    pub fn elements(&self, name: &str) -> Vec<Element> {
        let Some(value) = self.attribute(name) else {
            return Vec::new();
        };
        unsafe {
            if CFGetTypeID(value) != CFArrayGetTypeID() {
                CFRelease(value);
                return Vec::new();
            }
            let array = value as CFArrayRef;
            let count = CFArrayGetCount(array);
            let mut found = Vec::new();
            for at in 0..count {
                let item = CFArrayGetValueAtIndex(array, at);
                if item.is_null() {
                    continue;
                }
                // Read from an array with the get rule, so it has to be
                // retained before `Element` takes on releasing it.
                if let Some(element) = Element::owning(CFRetain(item)) {
                    found.push(element);
                }
            }
            CFRelease(value);
            found
        }
    }

    pub fn role(&self) -> Option<String> {
        self.text(ROLE)
    }

    pub fn title(&self) -> Option<String> {
        self.text(TITLE)
    }

    /// Whether this is something text can be typed into.
    pub fn is_text_input(&self) -> bool {
        self.role().is_some_and(|role| TEXT_ROLES.contains(&role.as_str()))
    }

    /// Puts the keyboard focus here.
    pub fn focus(&self) -> Result<(), String> {
        let key = CFString::new(FOCUSED);
        let yes = CFBoolean::true_value();
        let status = unsafe {
            AXUIElementSetAttributeValue(self.0, key.as_concrete_TypeRef(), yes.as_CFTypeRef())
        };
        if status == SUCCESS {
            Ok(())
        } else {
            Err(format!("AXFocused refused ({status})"))
        }
    }

    /// Brings this window to the front of its own application's windows.
    /// Which application is in front is a separate question — see
    /// `actions::open_app`.
    pub fn raise(&self) -> Result<(), String> {
        let action = CFString::new(RAISE);
        let status = unsafe { AXUIElementPerformAction(self.0, action.as_concrete_TypeRef()) };
        if status == SUCCESS {
            Ok(())
        } else {
            Err(format!("AXRaise refused ({status})"))
        }
    }
}

/// The accessibility element of a running application.
///
/// `None` in a test, and without the permission: an element is the only
/// way into this API, so refusing here closes it off everywhere at once.
pub fn application(pid: i32) -> Option<Element> {
    if cfg!(test) || !trusted() {
        return None;
    }
    unsafe { Element::owning(AXUIElementCreateApplication(pid)) }
}

/// An application that is running right now, as much of it as matching a
/// spoken name needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningApp {
    pub name: String,
    pub bundle_id: Option<String>,
    pub pid: i32,
}

/// The application in front, if there is one.
pub fn frontmost() -> Option<RunningApp> {
    if cfg!(test) {
        return None;
    }
    let app = NSWorkspace::sharedWorkspace().frontmostApplication()?;
    Some(RunningApp {
        name: app.localizedName().map(|n| n.to_string()).unwrap_or_default(),
        bundle_id: app.bundleIdentifier().map(|id| id.to_string()),
        pid: app.processIdentifier(),
    })
}

/// Every ordinary application running right now, in no particular order.
///
/// Accessory processes are left out: they have no windows anybody asks to
/// be taken to, and walking them is time spent on nothing.
pub fn running_apps() -> Vec<RunningApp> {
    if cfg!(test) {
        return Vec::new();
    }
    let mut found = Vec::new();
    for app in NSWorkspace::sharedWorkspace().runningApplications() {
        if app.activationPolicy() != NSApplicationActivationPolicy::Regular {
            continue;
        }
        found.push(RunningApp {
            name: app.localizedName().map(|n| n.to_string()).unwrap_or_default(),
            bundle_id: app.bundleIdentifier().map(|id| id.to_string()),
            pid: app.processIdentifier(),
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_api_is_closed_off_in_tests() {
        // The guard that keeps a test run from reaching into whatever
        // windows are open on the machine running it. Everything else in
        // this file needs an `Element`, and these are the only three ways
        // to get one.
        assert!(!trusted());
        assert!(application(1).is_none());
        assert!(frontmost().is_none());
        assert!(running_apps().is_empty());
    }
}
