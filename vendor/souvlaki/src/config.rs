use std::ffi::c_void;

/// OS-specific configuration needed to create media controls.
#[derive(Debug)]
pub struct PlatformConfig<'a> {
    /// The name to be displayed to the user. (*Required on Linux*)
    pub display_name: &'a str,
    /// Should follow [the D-Bus spec](https://dbus.freedesktop.org/doc/dbus-specification.html#message-protocol-names-bus). (*Required on Linux*)
    pub dbus_name: &'a str,
    /// ytm-rs: the app's .desktop file, without ".desktop" (MPRIS's `DesktopEntry`).
    /// GNOME's media controls bring the app up through it when clicked. (*Linux*)
    pub desktop_entry: Option<&'a str>,
    /// An HWND. (*Required on Windows*)
    pub hwnd: Option<*mut c_void>,
}
