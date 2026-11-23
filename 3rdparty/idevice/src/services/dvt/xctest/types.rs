//! NSKeyedArchive type proxies for the XCTest protocol.
//!
//! These types are exchanged as NSKeyedArchive-encoded plists between the IDE
//! and the on-device testmanagerd / test runner. They are distinct from the
//! DTX protocol itself and live here because they are XCTest-specific payloads.
//!
//! Types that are only ever *received* from the runner implement only decode
//! logic. [`XCTestConfiguration`] and [`XCTCapabilities`] must also be
//! encoded because the IDE sends them to the runner.
// Jackson Coxson

use plist::{Dictionary, Uid, Value};
use uuid::Uuid;

use crate::IdeviceError;

// ---------------------------------------------------------------------------
// Internal NSKeyedArchive encoder
// ---------------------------------------------------------------------------

/// Builds an NSKeyedArchive `$objects` array incrementally.
///
/// Each `encode_*` method appends one or more objects and returns the `Uid`
/// (index into `$objects`) of the newly added top-level entry. Call
/// [`ArchiveBuilder::finish`] to wrap the objects array into the complete
/// NSKeyedArchive plist dict.
struct ArchiveBuilder {
    objects: Vec<Value>,
}

impl ArchiveBuilder {
    fn new() -> Self {
        // $objects[0] is always the special "$null" sentinel
        Self {
            objects: vec![Value::String("$null".into())],
        }
    }

    /// Returns the UID that represents a null / missing value.
    fn null_uid() -> Uid {
        Uid::new(0)
    }

    /// Appends `v` to `$objects` and returns its index as a `Uid`.
    fn push(&mut self, v: Value) -> Uid {
        self.objects.push(v);
        Uid::new(self.objects.len() as u64 - 1)
    }

    /// Returns the UID of the class-info dict for `class_name`, creating it if
    /// it does not already exist in `$objects`.
    fn get_or_create_class(&mut self, class_name: &str, superclasses: &[&str]) -> Uid {
        // Reuse an existing class dict if present
        for (i, obj) in self.objects.iter().enumerate() {
            if let Some(d) = obj.as_dictionary()
                && d.get("$classname").and_then(|v| v.as_string()) == Some(class_name)
            {
                return Uid::new(i as u64);
            }
        }
        let mut classes: Vec<Value> = std::iter::once(class_name)
            .chain(superclasses.iter().copied())
            .map(|s| Value::String(s.into()))
            .collect();
        // Ensure NSObject is always the last entry
        if classes.last().and_then(|v| v.as_string()) != Some("NSObject") {
            classes.push(Value::String("NSObject".into()));
        }
        let mut d = Dictionary::new();
        d.insert("$classname".into(), Value::String(class_name.into()));
        d.insert("$classes".into(), Value::Array(classes));
        self.push(Value::Dictionary(d))
    }

    /// Encodes a `&str` as a plain NSString entry.
    fn encode_str(&mut self, s: &str) -> Uid {
        self.push(Value::String(s.into()))
    }

    /// Encodes an `Option<&str>`: `None` maps to `UID(0)` (null).
    fn encode_opt_str(&mut self, s: Option<&str>) -> Uid {
        match s {
            Some(s) => self.encode_str(s),
            None => Self::null_uid(),
        }
    }

    /// Encodes a boolean inline (not a UID reference).
    ///
    /// In NSKeyedArchive, scalar booleans are stored directly in the object
    /// dict, not as a UID reference into `$objects`.
    fn bool_value(b: bool) -> Value {
        Value::Boolean(b)
    }

    /// Encodes an integer inline.
    fn int_value(i: u64) -> Value {
        Value::Integer(i.into())
    }

    /// Encodes a `plist::Dictionary` as an `NSDictionary` object.
    fn encode_nsdict(&mut self, dict: &Dictionary) -> Uid {
        // Collect pairs first to avoid simultaneous borrow of self
        let pairs: Vec<(String, Value)> =
            dict.iter().map(|(k, v)| (k.clone(), v.clone())).collect();

        let mut key_uids = Vec::with_capacity(pairs.len());
        let mut val_uids = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            let k_uid = self.encode_str(&k);
            let v_uid = self.encode_value(v);
            key_uids.push(Value::Uid(k_uid));
            val_uids.push(Value::Uid(v_uid));
        }

        let class_uid = self.get_or_create_class("NSDictionary", &[]);
        let mut d = Dictionary::new();
        d.insert("$class".into(), Value::Uid(class_uid));
        d.insert("NS.keys".into(), Value::Array(key_uids));
        d.insert("NS.objects".into(), Value::Array(val_uids));
        self.push(Value::Dictionary(d))
    }

    /// Encodes a slice of `Value` as an `NSArray` object.
    fn encode_nsarray(&mut self, items: &[Value]) -> Uid {
        let items = items.to_vec();
        let mut obj_uids = Vec::with_capacity(items.len());
        for v in items {
            let uid = self.encode_value(v);
            obj_uids.push(Value::Uid(uid));
        }
        let class_uid = self.get_or_create_class("NSArray", &[]);
        let mut d = Dictionary::new();
        d.insert("$class".into(), Value::Uid(class_uid));
        d.insert("NS.objects".into(), Value::Array(obj_uids));
        self.push(Value::Dictionary(d))
    }

    /// Encodes an `NSURL` with a single relative string component.
    fn encode_nsurl(&mut self, url: &str) -> Uid {
        let str_uid = self.encode_str(url);
        let class_uid = self.get_or_create_class("NSURL", &[]);
        let mut d = Dictionary::new();
        d.insert("$class".into(), Value::Uid(class_uid));
        d.insert("NS.relative".into(), Value::Uid(str_uid));
        d.insert("NS.base".into(), Value::Uid(Self::null_uid()));
        self.push(Value::Dictionary(d))
    }

    /// Encodes an `NSUUID` from its raw 16-byte representation.
    fn encode_nsuuid(&mut self, uuid: &Uuid) -> Uid {
        let bytes = uuid.as_bytes().to_vec();
        let class_uid = self.get_or_create_class("NSUUID", &[]);
        let mut d = Dictionary::new();
        d.insert("$class".into(), Value::Uid(class_uid));
        d.insert("NS.uuidbytes".into(), Value::Data(bytes));
        self.push(Value::Dictionary(d))
    }

    /// Dispatches a generic `plist::Value` to the appropriate encoder.
    fn encode_value(&mut self, v: Value) -> Uid {
        match v {
            Value::Boolean(b) => self.push(Value::Boolean(b)),
            Value::Integer(i) => self.push(Value::Integer(i)),
            Value::Real(f) => self.push(Value::Real(f)),
            Value::String(s) => self.encode_str(&s),
            Value::Data(d) => self.push(Value::Data(d)),
            Value::Array(arr) => {
                let items = arr.clone();
                self.encode_nsarray(&items)
            }
            Value::Dictionary(d) => {
                let dict = d.clone();
                self.encode_nsdict(&dict)
            }
            // Unknown types map to $null
            _ => Self::null_uid(),
        }
    }

    /// Wraps `$objects` into a complete NSKeyedArchive plist dict.
    fn finish(self, root_uid: Uid) -> Value {
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(root_uid));

        let mut root = Dictionary::new();
        root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        root.insert("$version".into(), Value::Integer(100000u64.into()));
        root.insert("$top".into(), Value::Dictionary(top));
        root.insert("$objects".into(), Value::Array(self.objects));
        Value::Dictionary(root)
    }
}

/// Serialises an NSKeyedArchive `Value` to binary plist bytes.
fn archive_to_bytes(archive: Value) -> Result<Vec<u8>, IdeviceError> {
    let buf = Vec::new();
    let mut writer = std::io::BufWriter::new(buf);
    plist::to_writer_binary(&mut writer, &archive).map_err(|e| {
        tracing::warn!("Failed to serialise NSKeyedArchive: {e}");
        IdeviceError::UnexpectedResponse("failed to serialize NSKeyedArchive".into())
    })?;
    Ok(writer.into_inner().unwrap())
}

/// Serialises an `NSUUID` object to NSKeyedArchive bytes.
pub(crate) fn archive_nsuuid_to_bytes(uuid: &Uuid) -> Result<Vec<u8>, IdeviceError> {
    let mut class = Dictionary::new();
    class.insert(
        "$classes".into(),
        Value::Array(vec![Value::String("NSUUID".into())]),
    );
    class.insert("$classname".into(), Value::String("NSUUID".into()));

    let mut obj = Dictionary::new();
    obj.insert("$class".into(), Value::Uid(Uid::new(2)));
    obj.insert("NS.uuidbytes".into(), Value::Data(uuid.as_bytes().to_vec()));

    let mut top = Dictionary::new();
    top.insert("root".into(), Value::Uid(Uid::new(1)));

    let mut archive = Dictionary::new();
    archive.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    archive.insert(
        "$objects".into(),
        Value::Array(vec![
            Value::String("$null".into()),
            Value::Dictionary(obj),
            Value::Dictionary(class),
        ]),
    );
    archive.insert("$top".into(), Value::Dictionary(top));
    archive.insert("$version".into(), Value::Integer(100000u64.into()));

    archive_to_bytes(Value::Dictionary(archive))
}

/// Serialises an `XCTCapabilities` object to NSKeyedArchive bytes matching
/// pymobiledevice3's simple `encode_archive` layout.
pub(crate) fn archive_xct_capabilities_to_bytes(
    capabilities: &XCTCapabilities,
) -> Result<Vec<u8>, IdeviceError> {
    let mut objects = vec![Value::String("$null".into())];

    let root_uid = Uid::new(1);
    let xct_caps_class_uid = Uid::new(2);
    let dict_uid = Uid::new(3);
    let nsdict_class_uid = Uid::new(4);

    let mut root = Dictionary::new();
    root.insert("$class".into(), Value::Uid(xct_caps_class_uid));
    root.insert("capabilities-dictionary".into(), Value::Uid(dict_uid));
    objects.push(Value::Dictionary(root));

    let mut xct_caps_class = Dictionary::new();
    xct_caps_class.insert(
        "$classes".into(),
        Value::Array(vec![Value::String("XCTCapabilities".into())]),
    );
    xct_caps_class.insert("$classname".into(), Value::String("XCTCapabilities".into()));
    objects.push(Value::Dictionary(xct_caps_class));

    let key_base = 5u64;
    let mut key_uids = Vec::with_capacity(capabilities.capabilities.len());
    let mut value_uids = Vec::with_capacity(capabilities.capabilities.len());

    for (idx, (key, value)) in capabilities.capabilities.iter().enumerate() {
        let key_uid = Uid::new(key_base + (idx as u64 * 2));
        let value_uid = Uid::new(key_base + (idx as u64 * 2) + 1);
        key_uids.push(Value::Uid(key_uid));
        value_uids.push(Value::Uid(value_uid));
        objects.push(Value::String(key.clone()));
        objects.push(value.clone());
    }

    let mut dict = Dictionary::new();
    dict.insert("$class".into(), Value::Uid(nsdict_class_uid));
    dict.insert("NS.keys".into(), Value::Array(key_uids));
    dict.insert("NS.objects".into(), Value::Array(value_uids));
    objects.insert(dict_uid.get() as usize, Value::Dictionary(dict));

    let mut nsdict_class = Dictionary::new();
    nsdict_class.insert(
        "$classes".into(),
        Value::Array(vec![Value::String("NSDictionary".into())]),
    );
    nsdict_class.insert("$classname".into(), Value::String("NSDictionary".into()));
    objects.insert(
        nsdict_class_uid.get() as usize,
        Value::Dictionary(nsdict_class),
    );

    let mut top = Dictionary::new();
    top.insert("root".into(), Value::Uid(root_uid));

    let mut archive = Dictionary::new();
    archive.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    archive.insert("$objects".into(), Value::Array(objects));
    archive.insert("$top".into(), Value::Dictionary(top));
    archive.insert("$version".into(), Value::Integer(100000u64.into()));

    archive_to_bytes(Value::Dictionary(archive))
}

// ---------------------------------------------------------------------------
// XCTCapabilities
// ---------------------------------------------------------------------------

/// Proxy for `XCTCapabilities` — a dictionary wrapper negotiated between the
/// IDE and testmanagerd during session initialisation.
///
/// The default instance carries the set of capabilities that a modern Xcode
/// IDE advertises.
#[derive(Debug, Clone)]
pub struct XCTCapabilities {
    /// The inner `capabilities-dictionary` exchanged with the daemon.
    pub capabilities: Dictionary,
}

impl XCTCapabilities {
    /// Creates an empty `XCTCapabilities`.
    pub fn empty() -> Self {
        Self {
            capabilities: Dictionary::new(),
        }
    }

    /// Returns the default IDE capabilities advertised to testmanagerd.
    ///
    /// These match the values sent by a recent Xcode release and must be
    /// present for the modern DDI protocol variant to work correctly.
    pub fn ide_defaults() -> Self {
        let caps = crate::plist!(dict {
            "expected failure test capability": true,
            "test case run configurations": true,
            "test timeout capability": true,
            "test iterations": true,
            "request diagnostics for specific devices": true,
            "delayed attachment transfer": true,
            "skipped test capability": true,
            "daemon container sandbox extension": true,
            "ubiquitous test identifiers": true,
            "XCTIssue capability": true,
        });
        Self { capabilities: caps }
    }

    /// Decodes an `XCTCapabilities` from the `plist::Value` received in a DTX
    /// message payload (already decoded from NSKeyedArchive by the message
    /// layer).
    ///
    /// # Errors
    /// Returns `None` if the value does not contain a
    /// `"capabilities-dictionary"` key.
    pub fn from_plist(v: &Value) -> Option<Self> {
        let dict = v.as_dictionary()?;
        let caps = dict
            .get("capabilities-dictionary")
            .and_then(|v| v.as_dictionary())
            .cloned()
            .unwrap_or_default();
        Some(Self { capabilities: caps })
    }

    /// Converts to a `plist::Value` dict with the `capabilities-dictionary` wrapper.
    ///
    /// This is the form expected by `_IDE_initiateControlSessionWithCapabilities:` and
    /// `_IDE_initiateSessionWithIdentifier:capabilities:`.
    pub fn to_plist_value(&self) -> Value {
        let mut d = Dictionary::new();
        d.insert(
            "capabilities-dictionary".into(),
            Value::Dictionary(self.capabilities.clone()),
        );
        Value::Dictionary(d)
    }

    /// Encodes this `XCTCapabilities` into the provided [`ArchiveBuilder`] and
    /// returns the `Uid` of the resulting object entry.
    fn encode_with_builder(&self, builder: &mut ArchiveBuilder) -> Uid {
        let dict_uid = builder.encode_nsdict(&self.capabilities);
        let class_uid = builder.get_or_create_class("XCTCapabilities", &[]);
        let mut obj = Dictionary::new();
        obj.insert("$class".into(), Value::Uid(class_uid));
        obj.insert("capabilities-dictionary".into(), Value::Uid(dict_uid));
        builder.push(Value::Dictionary(obj))
    }
}

// ---------------------------------------------------------------------------
// XCTestConfiguration
// ---------------------------------------------------------------------------

/// Launch configuration for an XCTest runner bundle.
///
/// Built from [`TestConfig`](super::TestConfig) and serialised as an
/// NSKeyedArchive plist that is written to the device before the runner
/// process is launched.
///
/// All fields mirror the Objective-C `XCTestConfiguration` class.
#[derive(Debug, Clone)]
pub struct XCTestConfiguration {
    // --- required fields (no defaults) ------------------------------------
    /// `file://` URL pointing to the `.xctest` bundle inside the app container.
    pub test_bundle_url: String,
    /// UUID that uniquely identifies this test session.
    pub session_identifier: Uuid,

    // --- fields with per-run overrides ------------------------------------
    /// Module name used when `productModuleName` differs from the default.
    pub product_module_name: String,
    /// Path to the `XCTAutomationSupport.framework`.
    pub automation_framework_path: String,

    /// Bundle ID of the target application under test (optional).
    pub target_application_bundle_id: Option<String>,
    /// On-device path of the target application bundle (optional).
    pub target_application_path: Option<String>,
    /// Environment variables forwarded to the target app (optional).
    pub target_application_environment: Option<Dictionary>,
    /// Launch arguments forwarded to the target app.
    pub target_application_arguments: Vec<String>,

    /// Test identifiers to run; `None` means run all.
    pub tests_to_run: Option<Vec<String>>,
    /// Test identifiers to skip; `None` means skip none.
    pub tests_to_skip: Option<Vec<String>>,

    // --- fixed defaults ---------------------------------------------------
    /// IDE capabilities sent along with the configuration.
    pub ide_capabilities: XCTCapabilities,
}

impl XCTestConfiguration {
    /// Serialises this configuration as binary NSKeyedArchive bytes.
    ///
    /// The resulting bytes are written to `/tmp/<session-id>.xctestconfiguration`
    /// on the device via AFC before the runner process is launched.
    ///
    /// # Errors
    /// Returns [`IdeviceError::UnexpectedResponse`] if plist serialisation
    /// fails (should not happen under normal circumstances).
    pub fn to_archive_bytes(&self) -> Result<Vec<u8>, IdeviceError> {
        let mut b = ArchiveBuilder::new();

        // --- nested objects -----------------------------------------------

        let caps_uid = self.ide_capabilities.encode_with_builder(&mut b);
        let bundle_url_uid = b.encode_nsurl(&self.test_bundle_url);
        let session_uid = b.encode_nsuuid(&self.session_identifier);
        let automation_path_uid = b.encode_str(&self.automation_framework_path);
        let product_module_uid = b.encode_str(&self.product_module_name);

        // aggregateStatisticsBeforeCrash: {"XCSuiteRecordsKey": {}}
        let mut agg_stats = Dictionary::new();
        agg_stats.insert(
            "XCSuiteRecordsKey".into(),
            Value::Dictionary(Dictionary::new()),
        );
        let agg_stats_uid = b.encode_nsdict(&agg_stats);

        // targetApplicationPath — default placeholder keeps field non-empty
        let target_app_path_uid = b.encode_opt_str(
            self.target_application_path
                .as_deref()
                .or(Some("/whatever-it-does-not-matter/but-should-not-be-empty")),
        );

        let target_bundle_uid = b.encode_opt_str(self.target_application_bundle_id.as_deref());

        let target_env_uid = match &self.target_application_environment {
            Some(env) => b.encode_nsdict(env),
            None => ArchiveBuilder::null_uid(),
        };

        let target_args_uid = b.encode_nsarray(
            &self
                .target_application_arguments
                .iter()
                .map(|s| Value::String(s.clone()))
                .collect::<Vec<_>>(),
        );

        let tests_to_run_uid = match &self.tests_to_run {
            Some(t) => {
                let items: Vec<Value> = t.iter().map(|s| Value::String(s.clone())).collect();
                b.encode_nsarray(&items)
            }
            None => ArchiveBuilder::null_uid(),
        };

        let tests_to_skip_uid = match &self.tests_to_skip {
            Some(t) => {
                let items: Vec<Value> = t.iter().map(|s| Value::String(s.clone())).collect();
                b.encode_nsarray(&items)
            }
            None => ArchiveBuilder::null_uid(),
        };

        let format_version_uid = b.push(Value::Integer(2u64.into()));

        // --- XCTestConfiguration object dict ------------------------------

        let class_uid = b.get_or_create_class("XCTestConfiguration", &[]);

        let mut obj = Dictionary::new();

        // Nested-object fields (stored as UID references)
        obj.insert("$class".into(), Value::Uid(class_uid));
        obj.insert(
            "aggregateStatisticsBeforeCrash".into(),
            Value::Uid(agg_stats_uid),
        );
        obj.insert(
            "automationFrameworkPath".into(),
            Value::Uid(automation_path_uid),
        );
        obj.insert("IDECapabilities".into(), Value::Uid(caps_uid));
        obj.insert("productModuleName".into(), Value::Uid(product_module_uid));
        obj.insert(
            "targetApplicationArguments".into(),
            Value::Uid(target_args_uid),
        );
        obj.insert(
            "targetApplicationBundleID".into(),
            Value::Uid(target_bundle_uid),
        );
        obj.insert(
            "targetApplicationEnvironment".into(),
            Value::Uid(target_env_uid),
        );
        obj.insert(
            "targetApplicationPath".into(),
            Value::Uid(target_app_path_uid),
        );
        obj.insert("testBundleURL".into(), Value::Uid(bundle_url_uid));
        obj.insert("sessionIdentifier".into(), Value::Uid(session_uid));
        obj.insert("testsToRun".into(), Value::Uid(tests_to_run_uid));
        obj.insert("testsToSkip".into(), Value::Uid(tests_to_skip_uid));
        obj.insert("formatVersion".into(), Value::Uid(format_version_uid));

        // testApplicationDependencies: {} (empty NSDictionary, not null)
        let test_app_deps_uid = b.encode_nsdict(&Dictionary::new());
        obj.insert(
            "testApplicationDependencies".into(),
            Value::Uid(test_app_deps_uid),
        );

        // Null-valued optional fields
        for key in &[
            "baselineFileRelativePath",
            "baselineFileURL",
            "defaultTestExecutionTimeAllowance",
            "maximumTestExecutionTimeAllowance",
            "randomExecutionOrderingSeed",
            "testApplicationUserOverrides",
            "testBundleRelativePath",
        ] {
            obj.insert((*key).into(), Value::Uid(ArchiveBuilder::null_uid()));
        }

        // Inline boolean fields
        obj.insert(
            "disablePerformanceMetrics".into(),
            ArchiveBuilder::bool_value(false),
        );
        obj.insert("emitOSLogs".into(), ArchiveBuilder::bool_value(false));
        obj.insert(
            "gatherLocalizableStringsData".into(),
            ArchiveBuilder::bool_value(false),
        );
        obj.insert(
            "initializeForUITesting".into(),
            ArchiveBuilder::bool_value(true),
        );
        obj.insert("reportActivities".into(), ArchiveBuilder::bool_value(true));
        obj.insert(
            "reportResultsToIDE".into(),
            ArchiveBuilder::bool_value(true),
        );
        obj.insert(
            "testTimeoutsEnabled".into(),
            ArchiveBuilder::bool_value(false),
        );
        obj.insert("testsDrivenByIDE".into(), ArchiveBuilder::bool_value(false));
        obj.insert(
            "testsMustRunOnMainThread".into(),
            ArchiveBuilder::bool_value(true),
        );
        obj.insert(
            "treatMissingBaselinesAsFailures".into(),
            ArchiveBuilder::bool_value(false),
        );

        // Inline integer fields
        obj.insert(
            "systemAttachmentLifetime".into(),
            ArchiveBuilder::int_value(2),
        );
        obj.insert("testExecutionOrdering".into(), ArchiveBuilder::int_value(0));
        obj.insert(
            "userAttachmentLifetime".into(),
            ArchiveBuilder::int_value(0),
        );
        obj.insert(
            "preferredScreenCaptureFormat".into(),
            ArchiveBuilder::int_value(2),
        );

        let config_uid = b.push(Value::Dictionary(obj));
        let archive = b.finish(config_uid);
        archive_to_bytes(archive)
    }
}

// ---------------------------------------------------------------------------
// Runtime decode types (received from the runner, decode only)
// ---------------------------------------------------------------------------

/// Decoded proxy for `XCTTestIdentifier`.
///
/// `components` is the ordered list of name parts, e.g.
/// `["UITests", "testLogin"]`. Use [`test_class`](Self::test_class) and
/// [`test_method`](Self::test_method) as named accessors.
#[derive(Debug, Clone)]
pub struct XCTTestIdentifier {
    /// Ordered name components.
    pub components: Vec<String>,
}

impl XCTTestIdentifier {
    /// Returns the test class name (first component).
    pub fn test_class(&self) -> &str {
        self.components.first().map(|s| s.as_str()).unwrap_or("")
    }

    /// Returns the test method name (second component), if present.
    pub fn test_method(&self) -> Option<&str> {
        self.components.get(1).map(|s| s.as_str())
    }

    /// Decodes from a `plist::Value` received in a DTX payload.
    ///
    /// The value is expected to be a dictionary with a `"c"` key containing
    /// an array of component strings.
    pub fn from_plist(v: &Value) -> Option<Self> {
        let dict = v.as_dictionary()?;
        let components = dict
            .get("c")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_string().map(|s| s.to_owned()))
                    .collect()
            })
            .unwrap_or_default();
        Some(Self { components })
    }
}

/// Decoded proxy for `XCTSourceCodeLocation`.
#[derive(Debug, Clone)]
pub struct XCTSourceCodeLocation {
    /// `file://` URL string of the source file, or `None` if absent.
    pub file_url: Option<String>,
    /// Line number within the source file.
    pub line_number: u64,
}

impl XCTSourceCodeLocation {
    /// Returns the local file path, stripping the `file://` prefix if present.
    pub fn file_path(&self) -> Option<&str> {
        self.file_url
            .as_deref()
            .map(|u| u.strip_prefix("file://").unwrap_or(u))
    }

    /// Decodes from a `plist::Value` dict.
    pub fn from_plist(v: &Value) -> Option<Self> {
        let dict = v.as_dictionary()?;
        // `file-url` arrives as an NSURL object: {"NS.relative": "file://...", ...}.
        // Handle both the nested dict form and a plain string as a fallback.
        let file_url = dict.get("file-url").and_then(|v| {
            v.as_dictionary()
                .and_then(|d| d.get("NS.relative"))
                .and_then(|v| v.as_string())
                .map(|s| s.to_owned())
                .or_else(|| v.as_string().map(|s| s.to_owned()))
        });
        let line_number = dict
            .get("line-number")
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(0);
        Some(Self {
            file_url,
            line_number,
        })
    }
}

/// Decoded proxy for `XCTSourceCodeContext`.
#[derive(Debug, Clone)]
pub struct XCTSourceCodeContext {
    /// Source location, if available.
    pub location: Option<XCTSourceCodeLocation>,
}

impl XCTSourceCodeContext {
    /// Decodes from a `plist::Value` dict.
    pub fn from_plist(v: &Value) -> Option<Self> {
        let dict = v.as_dictionary()?;
        let location = dict
            .get("location")
            .and_then(XCTSourceCodeLocation::from_plist);
        Some(Self { location })
    }
}

/// Decoded proxy for `XCTIssue` / `XCTMutableIssue`.
///
/// `compact_description` is the short human-readable failure message
/// (e.g. `"((false) is true) failed"`).
#[derive(Debug, Clone)]
pub struct XCTIssue {
    /// Short failure description.
    pub compact_description: String,
    /// Detailed description, if available.
    pub detailed_description: Option<String>,
    /// Source location context, if available.
    pub source_code_context: Option<XCTSourceCodeContext>,
    /// Issue type code.
    pub issue_type: i64,
}

impl XCTIssue {
    /// Decodes from a `plist::Value` dict.
    pub fn from_plist(v: &Value) -> Option<Self> {
        let dict = v.as_dictionary()?;
        let compact = dict
            .get("compact-description")
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_owned();
        let detailed = dict
            .get("detailed-description")
            .and_then(|v| v.as_string())
            .map(|s| s.to_owned());
        let ctx = dict
            .get("source-code-context")
            .and_then(XCTSourceCodeContext::from_plist);
        let issue_type = dict
            .get("type")
            .and_then(|v| v.as_signed_integer())
            .unwrap_or(0);
        Some(Self {
            compact_description: compact,
            detailed_description: detailed,
            source_code_context: ctx,
            issue_type,
        })
    }
}

/// Decoded proxy for `XCActivityRecord` — a single activity step in a test.
#[derive(Debug, Clone)]
pub struct XCActivityRecord {
    /// Human-readable title of the activity.
    pub title: String,
    /// Activity type string.
    pub activity_type: String,
}

impl XCActivityRecord {
    /// Decodes from a `plist::Value` dict.
    pub fn from_plist(v: &Value) -> Option<Self> {
        let dict = v.as_dictionary()?;
        let title = dict
            .get("title")
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_owned();
        let activity_type = dict
            .get("activityType")
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_owned();
        Some(Self {
            title,
            activity_type,
        })
    }
}

/// Decoded proxy for `XCTestCaseRunConfiguration`.
#[derive(Debug, Clone, Copy)]
pub struct XCTestCaseRunConfiguration {
    /// Iteration index (1-based) when tests are repeated.
    pub iteration: u64,
}

impl XCTestCaseRunConfiguration {
    /// Decodes from a `plist::Value` dict.
    pub fn from_plist(v: &Value) -> Option<Self> {
        let dict = v.as_dictionary()?;
        let iteration = dict
            .get("iteration")
            .and_then(|v| v.as_unsigned_integer())
            .unwrap_or(1);
        Some(Self { iteration })
    }
}
