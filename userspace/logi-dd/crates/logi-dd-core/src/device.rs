use crate::error::{map_io_error, Error, Mode};
use crate::kind::Kind;
use crate::registry::{CLASSIC_REGISTRY, REGISTRY};
use crate::setting::{Access, Category, ModeReq, SettingSpec};
use crate::sysfs::{RealSysfs, SysfsIo};
use crate::value::Value;
use std::path::{Path, PathBuf};

/// Which physical wheel is connected, for frontends that need to brand the
/// UI (the Info/Testing page's product photo) rather than just render
/// settings generically. `Rs50`/`GPro` both use [`REGISTRY`] (the direct-
/// drive `wheel_*` attribute set; the two share one protocol, see the
/// project's G PRO protocol notes) and differ only in branding; `G923` uses
/// [`CLASSIC_REGISTRY`] instead, a different FFB engine entirely. `Unknown`
/// covers anything discovery could not pin to a specific product id (a dev-
/// override directory, an unrecognised future PID): it is treated as a DD
/// wheel for settings purposes, same as before this enum existed, and falls
/// back to the default (RS50) branding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WheelModel {
    #[default]
    Unknown,
    Rs50,
    GPro,
    G923,
}

impl WheelModel {
    /// The registry `self`'s settings live in: [`CLASSIC_REGISTRY`] for a
    /// G923, [`REGISTRY`] (the direct-drive `wheel_*` set) for everything
    /// else. A free function on the model alone (not `Device::settings`,
    /// which needs a live device) so a frontend holding only a
    /// [`DeviceInfo`]'s `model` field, not the `Device` itself, can still
    /// resolve which settings a wheel of this model would show.
    pub fn settings(self) -> &'static [SettingSpec] {
        match self {
            WheelModel::G923 => CLASSIC_REGISTRY,
            _ => REGISTRY,
        }
    }

    /// Whether `cat` has anything to show for a wheel of this model: at
    /// least one row in `self.settings()`, or `Info` (its identity rows can
    /// be empty on a classic wheel, but the page also carries the live
    /// input monitor and force-feedback test sims, which work off evdev
    /// regardless of model), or `Profiles` (its computer-side profile store
    /// snapshots whatever settings this model does have - see
    /// `crate::profiles` - so the category is never truly empty even for a
    /// wheel with no onboard slots at all, e.g. a G923). See
    /// [`Device::category_has_content`], which this backs; kept as a
    /// model-only free function for the same reason as
    /// [`WheelModel::settings`].
    pub fn category_has_content(self, cat: Category) -> bool {
        matches!(cat, Category::Info | Category::Profiles) || self.settings().iter().any(|s| s.category == cat)
    }
}

pub struct DeviceInfo {
    /// Human-readable identity for the Info page's "Wheel" row (e.g.
    /// "Logitech G923 Racing Wheel for PlayStation 4 and PC"), or empty
    /// when nothing could be resolved at all. See
    /// [`Device::info`]/[`wheel_display_name`].
    pub name: String,
    pub serial: String,
    /// The DD wheels' active-firmware string, read straight off
    /// `wheel_firmware` sysfs. A G923 has no such attribute, so this stays
    /// empty here even when the wheel's firmware IS known: that value
    /// takes a live HID++ round trip a plain `info()` read must not pay
    /// for on every call (draw loops call `info()` freely). Callers that
    /// want it explicitly opt in via [`Device::classic_firmware`], once
    /// per page load/refresh.
    pub firmware: String,
    pub mode: Mode,
    pub model: WheelModel,
}

pub struct Device<S: SysfsIo> {
    io: S,
    model: WheelModel,
    /// The interface-0 HID device directory a classic (G923) wheel's
    /// identity is anchored to: the sysfs `uniq` string lives in this same
    /// directory's `uevent` (see [`read_hid_uniq`]), and the HID++ vendor
    /// interface used for [`Device::classic_firmware`] is a sibling of it
    /// (see `hidpp::find_hidpp_sibling`). `None` for a DD wheel (identity
    /// comes from `wheel_serial`/`wheel_firmware` sysfs instead) and for
    /// every `Device` built via `with_io`/`with_io_and_model` (tests, and
    /// any caller with no real sysfs directory to anchor to).
    hid_dir: Option<PathBuf>,
}

/// The generic label used for the Info page's "Wheel" row when the wheel's
/// own evdev node cannot be found (a fresh connect can lag sysfs briefly,
/// or a dev-override fixture has no evdev at all): the model this crate
/// already knows, worded the same way the real device names itself.
fn generic_wheel_name(model: WheelModel) -> &'static str {
    match model {
        WheelModel::Rs50 => "Logitech RS50 Racing Wheel",
        WheelModel::GPro => "Logitech G PRO Racing Wheel",
        WheelModel::G923 => "Logitech G923 Racing Wheel",
        WheelModel::Unknown => "Logitech Racing Wheel",
    }
}

/// Human-readable wheel name for the Info page's "Wheel" row: the evdev
/// node's own name when the wheel is enumerated there (already fully
/// descriptive, e.g. "Logitech G923 Racing Wheel for PlayStation 4 and
/// PC"), else the generic label for `model` (evdev can lag sysfs briefly
/// right after a (re)connect, and a dev-override fixture has no evdev node
/// at all). Takes the scan directory explicitly so tests can point it at a
/// fixture instead of the real `/sys/class/input`.
pub fn wheel_display_name_at(sysfs_input: &Path, model: WheelModel) -> String {
    if let Some(input) = crate::evtest::scan_wheel_input(sysfs_input) {
        if !input.name.trim().is_empty() {
            return input.name;
        }
    }
    generic_wheel_name(model).to_string()
}

/// [`wheel_display_name_at`] against the real `/sys/class/input`.
pub fn wheel_display_name(model: WheelModel) -> String {
    wheel_display_name_at(Path::new("/sys/class/input"), model)
}

/// Parse `HID_UNIQ=` out of a HID device directory's `uevent`: there is no
/// dedicated `uniq` sysfs file on this kernel (checked live against a
/// G923), only this uevent key/value line. `None` when absent, empty, or
/// the directory has no `uevent` at all (a dev-override test fixture).
fn read_hid_uniq(dir: &Path) -> Option<String> {
    let uevent = std::fs::read_to_string(dir.join("uevent")).ok()?;
    uevent.lines().find_map(|line| {
        let v = line.strip_prefix("HID_UNIQ=")?.trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// USB product ids this crate can identify, mapped to their [`WheelModel`].
/// See `mainline/hid-logitech-hidpp.c`/`mainline/dd-lg4ff.c` for where each
/// id is bound on the kernel side.
fn model_from_pid(pid: u16) -> WheelModel {
    match pid {
        0xc276 => WheelModel::Rs50,
        0xc272 | 0xc268 => WheelModel::GPro,
        0xc266 | 0xc26e => WheelModel::G923,
        _ => WheelModel::Unknown,
    }
}

/// Parse the USB/HID product id out of a sysfs device directory name of the
/// form `BUS:VID:PID.SEQ` (the kernel's HID device naming convention, e.g.
/// `0003:046D:C266.0002`). `dir` is canonicalized first, since discovery's
/// `dir` may be (or resolve through symlinks to) a sibling hidraw node's own
/// `device` link rather than that exact directory. `None` for a name in a
/// different shape (a dev-override directory has no such name at all).
fn pid_from_hid_dir(dir: &std::path::Path) -> Option<u16> {
    let real = std::fs::canonicalize(dir).ok()?;
    let name = real.file_name()?.to_str()?;
    let mut parts = name.split(':');
    let _bus = parts.next()?;
    let _vid = parts.next()?;
    let pid_part = parts.next()?; // "C266.0002"
    let pid_hex = pid_part.split('.').next()?;
    u16::from_str_radix(pid_hex, 16).ok()
}

/// Whether `dir` looks like a G923-class classic sysfs surface: `range`,
/// `gain` and `autocenter` all present. `combine_pedals` is deliberately not
/// required here (older/trimmed ports could omit it), but the registry only
/// ever offers it when `available()` says so.
fn classic_attrs_present(dir: &std::path::Path) -> bool {
    dir.join("range").exists() && dir.join("gain").exists() && dir.join("autocenter").exists()
}

impl Device<RealSysfs> {
    /// Find the wheel by the sysfs attributes only this driver (or the
    /// classic G923 port sharing its kernel module) creates.
    ///
    /// `LOGI_DD_SYSFS_DIR`, when set, overrides discovery with a directory of
    /// attribute files (development aid: run the frontends against a
    /// plain-file copy of a device's sysfs dir, no wheel or driver needed).
    /// The directory must contain `wheel_range` (a DD wheel) or the classic
    /// `range`/`gain`/`autocenter` set (a G923) to count as a wheel, same as
    /// the real probe; a dev-override classic dir cannot be PID-checked (it
    /// is not a real HID device directory), so it is trusted and modeled as
    /// `G923`, the only classic wheel this crate knows.
    pub fn discover() -> Result<Device<RealSysfs>, Error> {
        if let Ok(dir) = std::env::var("LOGI_DD_SYSFS_DIR") {
            let dir = std::path::PathBuf::from(dir);
            if dir.join("wheel_range").exists() {
                return Ok(Device { io: RealSysfs::new(dir), model: WheelModel::Unknown, hid_dir: None });
            }
            if classic_attrs_present(&dir) {
                // Not a real HID device directory (no `uevent`/USB parent
                // structure), so the uniq/HID++ lookups this enables just
                // fail cleanly and the fixture shows "-"/"unavailable" -
                // exactly the fake-sysfs dev-aid's existing no-hidraw story.
                return Ok(Device {
                    io: RealSysfs::new(dir.clone()),
                    model: WheelModel::G923,
                    hid_dir: Some(dir),
                });
            }
            return Err(Error::NoWheel);
        }
        let mut entries = std::fs::read_dir("/sys/class/hidraw")
            .map_err(|_| Error::NoWheel)?;
        while let Some(Ok(e)) = entries.next() {
            let dir = e.path().join("device");
            if dir.join("wheel_range").exists() {
                let model = pid_from_hid_dir(&dir).map(model_from_pid).unwrap_or_default();
                return Ok(Device { io: RealSysfs::new(dir), model, hid_dir: None });
            }
            // Only trust the classic attr set when the PID confirms a real
            // G923: an unrelated device coincidentally exposing similarly-
            // named sysfs files must not be adopted as a wheel.
            if classic_attrs_present(&dir)
                && pid_from_hid_dir(&dir).map(model_from_pid) == Some(WheelModel::G923)
            {
                return Ok(Device {
                    io: RealSysfs::new(dir.clone()),
                    model: WheelModel::G923,
                    hid_dir: Some(dir),
                });
            }
        }
        Err(Error::NoWheel)
    }
}

impl<S: SysfsIo> Device<S> {
    pub fn with_io(io: S) -> Device<S> {
        Device { io, model: WheelModel::default(), hid_dir: None }
    }

    /// Same as `with_io`, but with an explicit `WheelModel` (tests, and any
    /// caller building a `Device` for a known-model classic wheel without
    /// going through `discover()`'s PID sniffing).
    pub fn with_io_and_model(io: S, model: WheelModel) -> Device<S> {
        Device { io, model, hid_dir: None }
    }

    pub fn model(&self) -> WheelModel {
        self.model
    }

    /// The registry this device's settings live in: [`CLASSIC_REGISTRY`] for
    /// a G923, [`REGISTRY`] (the direct-drive `wheel_*` set) for everything
    /// else. Frontends use this instead of the bare `REGISTRY` constant so a
    /// connected G923 only ever shows its own four settings, never the DD
    /// wheels' rows marked unavailable (a different device model, not "DD
    /// with everything missing").
    pub fn settings(&self) -> &'static [SettingSpec] {
        self.model.settings()
    }

    /// Look up `attr` in either registry: the two attribute namespaces never
    /// collide (the classic set has no `wheel_` prefix), so a plain attr
    /// lookup does not need to know which wheel is connected.
    pub fn spec(attr: &str) -> Option<&'static SettingSpec> {
        REGISTRY.iter().find(|s| s.attr == attr).or_else(|| CLASSIC_REGISTRY.iter().find(|s| s.attr == attr))
    }

    pub fn available(&self, attr: &str) -> bool {
        self.io.exists(attr)
    }

    /// Whether `cat` has anything to show for this device: at least one row
    /// in `self.settings()`, or `Info` (its identity rows can be empty on a
    /// classic wheel, but the page also carries the live input monitor and
    /// force-feedback test sims, which work off evdev regardless of model).
    /// Frontends use this to hide a sidebar entry that would otherwise open
    /// onto a blank page, e.g. a G923 has no `Leds`/`Profiles` rows at all.
    pub fn category_has_content(&self, cat: Category) -> bool {
        self.model.category_has_content(cat)
    }

    /// The classic engine (a G923) has no desktop/onboard split at all, so
    /// it always reads as `Desktop`: the mode gating every caller applies
    /// (`ModeReq`) is `Any` for every classic setting, but `current_mode`
    /// itself must still resolve rather than error, since
    /// `info()`/`ensure_desktop_mode()`/`drift_snapshot()` all call it
    /// unconditionally regardless of which wheel is connected. This is
    /// gated on the model, not just on `wheel_mode`'s absence: for a DD
    /// wheel, a missing `wheel_mode` means the wheel is actually gone (the
    /// no-wheel/drift-detection paths rely on that read failing then), so
    /// only a confirmed classic device gets the free pass.
    pub fn current_mode(&self) -> Result<Mode, Error> {
        if self.model == WheelModel::G923 {
            return Ok(Mode::Desktop);
        }
        match self.io.read("wheel_mode").map_err(|e| map_io_error(&e, "wheel_mode"))?.trim() {
            "onboard" => Ok(Mode::Onboard),
            _ => Ok(Mode::Desktop),
        }
    }

    pub fn info(&self) -> Result<DeviceInfo, Error> {
        let read = |a: &str| {
            self.io.read(a).map(|s| s.trim().to_string()).unwrap_or_default()
        };
        // A G923 has no `wheel_serial` sysfs at all; its serial is the HID
        // `uniq` string off the same interface-0 directory `hid_dir`
        // anchors to instead (cheap: one small file read, safe to do on
        // every `info()` call, unlike `classic_firmware`'s live HID++
        // round trip).
        let serial = if self.model == WheelModel::G923 {
            self.hid_dir.as_deref().and_then(read_hid_uniq).unwrap_or_default()
        } else {
            read("wheel_serial")
        };
        Ok(DeviceInfo {
            name: wheel_display_name(self.model),
            serial,
            // The driver returns "base: ...\nmotor: ..."; keep it on one line.
            firmware: read("wheel_firmware").replace('\n', " / "),
            mode: self.current_mode()?,
            model: self.model,
        })
    }

    /// Best-effort HID++ firmware string for a classic (G923) wheel: `None`
    /// immediately for any other model (nothing to query; `info().firmware`
    /// already covers them from sysfs), and `None` when the HID++ sibling
    /// node or the query itself failed (unavailable permissions, timeout,
    /// unplugged). This is a real USB round trip with its own timeout, not
    /// a sysfs read: callers must call it once per Info-page load/refresh,
    /// never per frame/draw (see `hidpp::query_g923_firmware`).
    pub fn classic_firmware(&self) -> Option<String> {
        if self.model != WheelModel::G923 {
            return None;
        }
        crate::hidpp::query_g923_firmware(self.hid_dir.as_deref()?)
    }

    pub fn read(&self, attr: &str) -> Result<Value, Error> {
        let spec = Self::spec(attr).ok_or(Error::Invalid)?;
        // Action attributes are write-only triggers; reading the sysfs file
        // returns EACCES. Report the trigger value instead of a permission error.
        if spec.access == Access::Action {
            return Ok(Value::Trigger);
        }
        let raw = self.io.read(attr).map_err(|e| map_io_error(&e, attr))?;
        // wheel_mode / wheel_texture_route report words; map to the enum index.
        if let Kind::Enum(variants) = spec.kind {
            let t = raw.trim();
            if let Some(i) = variants.iter().position(|v| *v == t) {
                return Ok(Value::Enum(i as u8));
            }
        }
        spec.kind.parse(&raw)
    }

    pub fn write(&self, attr: &str, v: &Value) -> Result<(), Error> {
        let spec = Self::spec(attr).ok_or(Error::Invalid)?;
        if spec.access == Access::ReadOnly {
            return Err(Error::Invalid);
        }
        spec.kind.validate(v)?;
        // Mode gating: reject up front with a WrongMode the UI can act on.
        match spec.mode_req {
            ModeReq::DesktopOnly if self.current_mode()? != Mode::Desktop => {
                return Err(Error::WrongMode { needed: Mode::Desktop });
            }
            ModeReq::OnboardOnly if self.current_mode()? != Mode::Onboard => {
                return Err(Error::WrongMode { needed: Mode::Onboard });
            }
            _ => {}
        }
        let raw = self.raw_for_write(spec, v)?;
        self.io.write(attr, &raw).map_err(|e| map_io_error(&e, attr))
    }

    /// wheel_mode/texture_route take words; write the variant string, not index.
    fn raw_for_write(&self, spec: &SettingSpec, v: &Value) -> Result<String, Error> {
        if let (Kind::Enum(variants), Value::Enum(i)) = (spec.kind, v) {
            if spec.attr == "wheel_mode" || spec.attr == "wheel_texture_route" {
                return variants
                    .get(*i as usize)
                    .map(|s| s.to_string())
                    .ok_or(Error::OutOfRange);
            }
        }
        spec.kind.format(v)
    }

    pub fn ensure_desktop_mode(&self) -> Result<(), Error> {
        if self.current_mode()? == Mode::Desktop {
            return Ok(());
        }
        self.io.write("wheel_mode", "desktop").map_err(|e| map_io_error(&e, "wheel_mode"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysfs::FakeSysfs;
    use crate::value::Value;

    fn dev() -> Device<FakeSysfs> {
        let fs = FakeSysfs::new();
        fs.set("wheel_range", "900");
        fs.set("wheel_mode", "desktop");
        fs.set("wheel_serial", "2538WDQ0M9X8");
        fs.set("wheel_sensitivity", "50");
        fs.set("wheel_texture_route", "tf");
        Device::with_io(fs)
    }

    #[test]
    fn reads_typed_value() {
        assert_eq!(dev().read("wheel_range").unwrap(), Value::Int(900));
    }

    #[test]
    fn texture_route_word_parses_to_enum() {
        // driver reports "tf"; registry models it as Enum index 1
        assert_eq!(dev().read("wheel_texture_route").unwrap(), Value::Enum(1));
    }

    #[test]
    fn action_attrs_read_as_trigger_not_permission_error() {
        // wheel_led_apply / wheel_calibrate_here are write-only (0220); reading
        // the file gives EACCES. read() must report the trigger, not the error.
        let fs = FakeSysfs::new();
        fs.set_errno("wheel_led_apply", 13); // EACCES if it tried to read
        let d = Device::with_io(fs);
        assert_eq!(d.read("wheel_led_apply").unwrap(), Value::Trigger);
        // even with the file entirely absent
        assert_eq!(d.read("wheel_calibrate_here").unwrap(), Value::Trigger);
    }

    #[test]
    fn firmware_info_is_single_line() {
        let fs = FakeSysfs::new();
        fs.set("wheel_mode", "desktop");
        fs.set("wheel_serial", "X");
        fs.set("wheel_firmware", "base: U1 65.04.B0039\nmotor: SC 02.01.B0042\n");
        let info = Device::with_io(fs).info().unwrap();
        assert!(!info.firmware.contains('\n'), "firmware: {:?}", info.firmware);
        assert_eq!(info.firmware, "base: U1 65.04.B0039 / motor: SC 02.01.B0042");
    }

    #[test]
    fn writes_valid_value() {
        let d = dev();
        d.write("wheel_range", &Value::Int(540)).unwrap();
        assert_eq!(d.read("wheel_range").unwrap(), Value::Int(540));
    }

    #[test]
    fn write_out_of_range_rejected_before_io() {
        let d = dev();
        assert!(matches!(d.write("wheel_range", &Value::Int(45)), Err(Error::OutOfRange)));
    }

    #[test]
    fn desktop_only_write_in_onboard_returns_wrong_mode() {
        let fs = FakeSysfs::new();
        fs.set("wheel_mode", "onboard");
        fs.set("wheel_sensitivity", "50");
        let d = Device::with_io(fs);
        assert!(matches!(d.write("wheel_sensitivity", &Value::Percent(10)),
                         Err(Error::WrongMode { needed: Mode::Desktop })));
    }

    #[test]
    fn ensure_desktop_switches_mode() {
        let fs = FakeSysfs::new();
        fs.set("wheel_mode", "onboard");
        let d = Device::with_io(fs);
        d.ensure_desktop_mode().unwrap();
        assert_eq!(d.current_mode().unwrap(), Mode::Desktop);
    }

    #[test]
    fn available_reflects_presence() {
        let d = dev();
        assert!(d.available("wheel_range"));
        assert!(!d.available("wheel_brake_force"));
    }

    #[test]
    fn info_reads_identity() {
        let i = dev().info().unwrap();
        assert_eq!(i.serial, "2538WDQ0M9X8");
        assert_eq!(i.mode, Mode::Desktop);
    }

    // --- G923 / WheelModel ---

    /// Whether `a` and `b` are the same registry, by content (attr names,
    /// in order) rather than address: `pub const X: &[T] = &[..]` is a
    /// `const`, not a `static`, so two syntactic uses of the same constant
    /// are not guaranteed to share one address (each use may promote its
    /// own anonymous static), making `std::ptr::eq` an unreliable way to
    /// check "is this the registry I expect".
    fn same_registry(a: &[SettingSpec], b: &[SettingSpec]) -> bool {
        a.iter().map(|s| s.attr).eq(b.iter().map(|s| s.attr))
    }

    #[test]
    fn with_io_defaults_to_unknown_model_and_the_dd_registry() {
        let d = Device::with_io(FakeSysfs::new());
        assert_eq!(d.model(), WheelModel::Unknown);
        assert!(same_registry(d.settings(), REGISTRY));
    }

    #[test]
    fn a_g923_device_uses_the_classic_registry() {
        let fs = FakeSysfs::new();
        fs.set("range", "900");
        fs.set("gain", "65535");
        fs.set("autocenter", "0");
        fs.set("combine_pedals", "0");
        let d = Device::with_io_and_model(fs, WheelModel::G923);
        assert!(same_registry(d.settings(), CLASSIC_REGISTRY));
        assert_eq!(d.read("range").unwrap(), Value::Int(900));
        // gain/autocenter read back as a percent (Kind::ScaledPercent), not
        // the raw sysfs 0-65535 integer.
        assert_eq!(d.read("gain").unwrap(), Value::Percent(100));
        assert_eq!(d.read("combine_pedals").unwrap(), Value::Enum(0));
    }

    #[test]
    fn a_g923_device_writes_and_validates_its_settings() {
        let fs = FakeSysfs::new();
        fs.set("range", "900");
        fs.set("gain", "0");
        fs.set("autocenter", "0");
        fs.set("combine_pedals", "0");
        let d = Device::with_io_and_model(fs, WheelModel::G923);
        d.write("range", &Value::Int(540)).unwrap();
        assert_eq!(d.read("range").unwrap(), Value::Int(540));
        assert!(matches!(d.write("range", &Value::Int(39)), Err(Error::OutOfRange)));
        assert!(matches!(d.write("range", &Value::Int(901)), Err(Error::OutOfRange)));
        d.write("combine_pedals", &Value::Enum(2)).unwrap();
        assert_eq!(d.read("combine_pedals").unwrap(), Value::Enum(2));
        // gain is written as a percent; the raw sysfs attr underneath still
        // takes the scaled 0-65535 value (Oversteer compatibility).
        d.write("gain", &Value::Percent(50)).unwrap();
        assert_eq!(d.read("gain").unwrap(), Value::Percent(50));
    }

    #[test]
    fn a_g923_device_has_no_dd_settings_available() {
        // The registry selection is exclusive: a G923's `settings()` never
        // includes the DD wheels' `wheel_*` rows, so a frontend iterating it
        // never renders them (not even as "unavailable").
        let d = Device::with_io_and_model(FakeSysfs::new(), WheelModel::G923);
        assert!(!d.settings().iter().any(|s| s.attr.starts_with("wheel_")));
        assert!(d.settings().iter().any(|s| s.attr == "range"));
    }

    #[test]
    fn a_g923_has_no_content_for_leds_but_info_and_profiles_always_have_content() {
        // A classic wheel has no LIGHTSYNC strip at all: that sidebar page
        // would be blank, so frontends must hide it rather than open onto
        // nothing. Info always has content (the live input monitor + test
        // sims work regardless of model), and so does Profiles: even with
        // no onboard slots, the computer-side profile store still has the
        // wheel's own four settings to save/apply.
        let d = Device::with_io_and_model(FakeSysfs::new(), WheelModel::G923);
        assert!(!d.category_has_content(Category::Leds));
        assert!(d.category_has_content(Category::Profiles));
        assert!(d.category_has_content(Category::Info));
        assert!(d.category_has_content(Category::Ffb));
        assert!(d.category_has_content(Category::Steering));
        assert!(d.category_has_content(Category::Pedals));
    }

    #[test]
    fn a_dd_wheel_has_content_in_every_category() {
        let d = Device::with_io_and_model(FakeSysfs::new(), WheelModel::Rs50);
        for cat in Category::ALL {
            assert!(d.category_has_content(*cat), "{cat:?}");
        }
    }

    #[test]
    fn a_classic_wheel_with_no_wheel_mode_reads_as_desktop() {
        // The classic engine has no onboard/desktop split at all; current_mode
        // must resolve rather than error so info()/writes never fail on it.
        let fs = FakeSysfs::new();
        fs.set("range", "900");
        let d = Device::with_io_and_model(fs, WheelModel::G923);
        assert_eq!(d.current_mode().unwrap(), Mode::Desktop);
        let info = d.info().unwrap();
        assert_eq!(info.mode, Mode::Desktop);
        assert_eq!(info.model, WheelModel::G923);
        // A classic wheel has no wheel_serial/wheel_firmware sysfs either;
        // info() must still succeed with blank identity rather than erroring.
        assert_eq!(info.serial, "");
        assert_eq!(info.firmware, "");
    }

    #[test]
    fn ensure_desktop_mode_is_a_no_op_without_wheel_mode() {
        let d = Device::with_io_and_model(FakeSysfs::new(), WheelModel::G923);
        // No wheel_mode attr to write; must not error and must not panic.
        d.ensure_desktop_mode().unwrap();
    }

    #[test]
    fn spec_resolves_attrs_from_either_registry() {
        assert_eq!(Device::<FakeSysfs>::spec("wheel_range").unwrap().attr, "wheel_range");
        assert_eq!(Device::<FakeSysfs>::spec("range").unwrap().attr, "range");
        assert_eq!(Device::<FakeSysfs>::spec("combine_pedals").unwrap().attr, "combine_pedals");
        assert!(Device::<FakeSysfs>::spec("nonexistent").is_none());
    }

    #[test]
    fn model_from_pid_maps_the_known_product_ids() {
        assert_eq!(model_from_pid(0xc276), WheelModel::Rs50);
        assert_eq!(model_from_pid(0xc272), WheelModel::GPro);
        assert_eq!(model_from_pid(0xc268), WheelModel::GPro);
        assert_eq!(model_from_pid(0xc266), WheelModel::G923);
        assert_eq!(model_from_pid(0xc26e), WheelModel::G923);
        assert_eq!(model_from_pid(0x1234), WheelModel::Unknown);
    }

    #[test]
    fn pid_from_hid_dir_parses_the_kernel_naming_convention() {
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-hid-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let hid_dir = dir.join("0003:046D:C266.0002");
        std::fs::create_dir_all(&hid_dir).unwrap();
        assert_eq!(pid_from_hid_dir(&hid_dir), Some(0xc266));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pid_from_hid_dir_is_none_for_an_unshaped_directory() {
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-plain-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(pid_from_hid_dir(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn classic_attrs_present_requires_all_three_files() {
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-classic-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!classic_attrs_present(&dir));
        std::fs::write(dir.join("range"), "900").unwrap();
        std::fs::write(dir.join("gain"), "0").unwrap();
        assert!(!classic_attrs_present(&dir), "autocenter still missing");
        std::fs::write(dir.join("autocenter"), "0").unwrap();
        assert!(classic_attrs_present(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// `discover()`'s `LOGI_DD_SYSFS_DIR` dev-override path, both shapes, in
    /// one test: the only test in this crate touching that variable, so it
    /// cannot race another test over it (two separate tests both setting it
    /// could race each other under the default parallel test runner).
    #[test]
    fn discover_dev_override_recognizes_both_directory_shapes() {
        let base = std::env::temp_dir().join(format!(
            "logi-dd-device-test-discover-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));

        let classic_dir = base.join("classic");
        std::fs::create_dir_all(&classic_dir).unwrap();
        std::fs::write(classic_dir.join("range"), "900").unwrap();
        std::fs::write(classic_dir.join("gain"), "0").unwrap();
        std::fs::write(classic_dir.join("autocenter"), "0").unwrap();
        std::env::set_var("LOGI_DD_SYSFS_DIR", &classic_dir);
        let classic = Device::discover().unwrap();
        assert_eq!(classic.model(), WheelModel::G923);
        assert_eq!(classic.read("range").unwrap(), Value::Int(900));

        let dd_dir = base.join("dd");
        std::fs::create_dir_all(&dd_dir).unwrap();
        std::fs::write(dd_dir.join("wheel_range"), "900").unwrap();
        std::env::set_var("LOGI_DD_SYSFS_DIR", &dd_dir);
        let dd = Device::discover().unwrap();
        assert_eq!(dd.model(), WheelModel::Unknown);
        assert!(same_registry(dd.settings(), REGISTRY));

        std::env::remove_var("LOGI_DD_SYSFS_DIR");
        std::fs::remove_dir_all(&base).unwrap();
    }

    // --- Wheel row identity: name, and the G923's uniq-based serial ---

    #[test]
    fn generic_wheel_name_covers_every_model() {
        assert_eq!(generic_wheel_name(WheelModel::Rs50), "Logitech RS50 Racing Wheel");
        assert_eq!(generic_wheel_name(WheelModel::GPro), "Logitech G PRO Racing Wheel");
        assert_eq!(generic_wheel_name(WheelModel::G923), "Logitech G923 Racing Wheel");
        assert_eq!(generic_wheel_name(WheelModel::Unknown), "Logitech Racing Wheel");
    }

    #[test]
    fn wheel_display_name_at_prefers_the_evdev_name_when_found() {
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-wheelname-found-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let event_dir = dir.join("event7").join("device");
        std::fs::create_dir_all(&event_dir).unwrap();
        std::fs::write(
            event_dir.join("name"),
            "Logitech G923 Racing Wheel for PlayStation 4 and PC\n",
        )
        .unwrap();
        assert_eq!(
            wheel_display_name_at(&dir, WheelModel::G923),
            "Logitech G923 Racing Wheel for PlayStation 4 and PC"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn wheel_display_name_at_falls_back_to_the_model_when_evdev_has_nothing() {
        // A directory with no matching event node (evdev lagging a fresh
        // connect, or simply no wheel plugged in at all).
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-wheelname-missing-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(wheel_display_name_at(&dir, WheelModel::Rs50), "Logitech RS50 Racing Wheel");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_hid_uniq_parses_the_uevent_line() {
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-uniq-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("uevent"),
            "DRIVER=logitech-dd\nHID_ID=0003:0000046D:0000C266\nHID_UNIQ=FAKE0000SERIAL\nMODALIAS=x\n",
        )
        .unwrap();
        assert_eq!(read_hid_uniq(&dir).as_deref(), Some("FAKE0000SERIAL"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_hid_uniq_is_none_when_absent_or_blank() {
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-uniq-blank-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // No uevent file at all (a dev-override fixture).
        assert_eq!(read_hid_uniq(&dir), None);
        std::fs::write(dir.join("uevent"), "DRIVER=x\nHID_UNIQ=\n").unwrap();
        assert_eq!(read_hid_uniq(&dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_g923s_serial_comes_from_hid_uniq_not_wheel_serial() {
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-g923-serial-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("uevent"), "HID_UNIQ=FAKESERIAL01\n").unwrap();
        let fs = FakeSysfs::new();
        fs.set("range", "900");
        let d = Device { io: fs, model: WheelModel::G923, hid_dir: Some(dir.clone()) };
        let info = d.info().unwrap();
        assert_eq!(info.serial, "FAKESERIAL01");
        assert_eq!(info.model, WheelModel::G923);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_g923_without_a_hid_dir_has_a_blank_serial_not_a_panic() {
        // `with_io_and_model` (no real sysfs directory behind it) leaves
        // `hid_dir` at `None`; `info()` must still succeed.
        let d = Device::with_io_and_model(FakeSysfs::new(), WheelModel::G923);
        let info = d.info().unwrap();
        assert_eq!(info.serial, "");
    }

    #[test]
    fn classic_firmware_is_none_for_every_non_g923_model() {
        for model in [WheelModel::Unknown, WheelModel::Rs50, WheelModel::GPro] {
            let d = Device::with_io_and_model(FakeSysfs::new(), model);
            assert!(d.classic_firmware().is_none(), "{model:?}");
        }
    }

    #[test]
    fn classic_firmware_is_none_without_a_hid_dir_to_anchor_to() {
        let d = Device::with_io_and_model(FakeSysfs::new(), WheelModel::G923);
        assert!(d.classic_firmware().is_none());
    }

    #[test]
    fn classic_firmware_is_none_for_a_dev_override_style_fixture() {
        // A `hid_dir` with no real USB parent structure at all (matching
        // `discover()`'s dev-override path): the HID++ sibling walk must
        // fail cleanly, not panic, and `info()`'s cheap fields stay usable.
        let dir = std::env::temp_dir().join(format!(
            "logi-dd-device-test-g923-nohidpp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let fs = FakeSysfs::new();
        fs.set("range", "900");
        let d = Device { io: fs, model: WheelModel::G923, hid_dir: Some(dir.clone()) };
        assert!(d.classic_firmware().is_none());
        assert!(d.info().is_ok(), "the cheap identity fields must not be affected");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
