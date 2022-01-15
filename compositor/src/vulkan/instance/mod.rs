mod error;

use std::{
    ffi::{CStr, CString, NulError},
    fmt::{self, Formatter},
    sync::Arc,
};

use ash::vk::{ApplicationInfo, InstanceCreateInfo};

use super::{version::Version, LIBRARY, SMITHAY_VERSION};

pub use self::error::*;

/// A builder used to construct an [`Instance`].
///
/// To instantiate, use [`Instance::builder`].
#[derive(Debug)]
pub struct InstanceBuilder {
    api_version: Version,
    enable_extensions: Vec<String>,
    enable_layers: Vec<String>,
    app_name: Option<String>,
    app_version: Option<Version>,
}

impl InstanceBuilder {
    /// Sets the API version that should be used when creating an instance.
    ///
    /// The default value is [`Version::VERSION_1_0`].
    ///
    /// You should ensure the version you are requesting is supported using [`Instance::max_instance_version`].
    pub fn api_version(mut self, version: Version) -> InstanceBuilder {
        self.api_version = version;
        self
    }

    /// Adds an instance extension to be requested when creating an [`Instance`].
    ///
    /// The extension must be supported by the Vulkan runtime or else building the instance will fail. A great way to
    /// ensure the extension you are requesting is supported is to check if your extension is listed in
    /// [`Instance::enumerate_extensions`].
    pub fn extension(mut self, extension: impl Into<String>) -> InstanceBuilder {
        self.enable_extensions.push(extension.into());
        self
    }

    /// Adds an instance layer to be requested when creating an [`Instance`].
    ///
    /// The layer must be supported by the Vulkan runtime or else building the instance will fail. A great way to
    /// ensure the layer you are requesting is supported is to check if your layer is listed in [`Instance::enumerate_layers`].
    pub fn layer(mut self, layer: impl Into<String>) -> InstanceBuilder {
        self.enable_layers.push(layer.into());
        self
    }

    /// Sets the app name that will be used by the driver when creating an instance.
    pub fn app_name(mut self, name: impl Into<String>) -> InstanceBuilder {
        self.app_name = Some(name.into());
        self
    }

    /// Sets the app version that will be used by the driver when creating an instance.
    pub fn app_version(mut self, version: Version) -> InstanceBuilder {
        self.app_version = Some(version);
        self
    }

    /// Creates an instance using this builder.
    pub fn build(self) -> Result<Instance, InstanceError> {
        // Check if the requested extensions and layers are supported.
        let supported_layers = Instance::enumerate_layers()?.collect::<Vec<_>>();
        let supported_extensions = Instance::enumerate_extensions()?.collect::<Vec<_>>();

        let missing_layers = self
            .enable_layers
            .iter()
            // Filter out entries that are present.
            .filter(|s| !supported_layers.contains(s))
            .cloned()
            .collect::<Vec<_>>();

        if !missing_layers.is_empty() {
            return Err(InstanceError::MissingLayers(missing_layers));
        }

        let missing_extensions = self
            .enable_extensions
            .iter()
            // Filter out entries that are present.
            .filter(|s| !supported_extensions.contains(s))
            .cloned()
            .collect::<Vec<_>>();

        if !missing_extensions.is_empty() {
            return Err(InstanceError::MissingExtensions(missing_extensions));
        }

        // We cannot immediately go to a Vec<*const c_char> since that will cause the CString drop impl to
        // be called and our resulting pointers will have been freed.
        let extensions = self
            .enable_extensions
            .iter()
            .map(|s| CString::new(s.clone()))
            .collect::<Result<Vec<_>, NulError>>()
            .expect("Non UTF-8 extension string");

        let layers = self
            .enable_layers
            .iter()
            .map(|s| CString::new(s.clone()))
            .collect::<Result<Vec<_>, NulError>>()
            .expect("Non UTF-8 layer string");

        let mut app_info = ApplicationInfo::builder()
            .api_version(self.api_version.to_raw())
            .engine_name(unsafe { CStr::from_bytes_with_nul_unchecked(b"Smithay\0") })
            .engine_version(SMITHAY_VERSION.to_raw());

        if let Some(app_version) = self.app_version {
            app_info = app_info.application_version(app_version.to_raw());
        }

        let app_name = self
            .app_name
            .map(|name| CString::new(name).expect("app name contains null terminator"));

        if let Some(app_name) = &app_name {
            app_info = app_info.application_name(app_name);
        }

        let layer_ptrs = layers.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();
        let extension_ptrs = extensions.iter().map(|s| s.as_ptr()).collect::<Vec<_>>();

        let create_info = InstanceCreateInfo::builder()
            .application_info(&app_info)
            .enabled_layer_names(&layer_ptrs[..])
            .enabled_extension_names(&extension_ptrs[..]);

        let instance = unsafe { LIBRARY.create_instance(&create_info, None) }?;
        let instance = Arc::new(InstanceInner {
            instance,
            version: self.api_version,
        });

        Ok(instance.into())
    }
}

/// A Vulkan instance which allows interfacing with the Vulkan APIs.
#[derive(Debug)]
pub struct Instance(pub(crate) Arc<InstanceInner>);

impl Instance {
    /// Returns the max Vulkan API version supported any created instances.
    pub fn max_instance_version() -> Result<Version, InstanceError> {
        let version = LIBRARY
            .try_enumerate_instance_version()?
            .map(Version::from_raw)
            // Vulkan 1.0 does not have `vkEnumerateInstanceVersion`.
            .unwrap_or(Version::VERSION_1_0);

        Ok(version)
    }

    /// Enumerates over the available instance layers on the system.
    pub fn enumerate_layers() -> Result<impl Iterator<Item = String>, InstanceError> {
        let layers = LIBRARY
            .enumerate_instance_layer_properties()?
            .into_iter()
            .map(|properties| {
                // SAFETY: String is null terminated.
                let c_str = unsafe { CStr::from_ptr(&properties.layer_name as *const _) };
                c_str.to_str().expect("Invalid UTF-8 in layer name").to_owned()
            });

        Ok(layers)
    }

    /// Enumerates over the available instance layers on the system.
    pub fn enumerate_extensions() -> Result<impl Iterator<Item = String>, InstanceError> {
        let extensions = LIBRARY
            .enumerate_instance_extension_properties()?
            .into_iter()
            .map(|properties| {
                // SAFETY: String is null terminated.
                let c_str = unsafe { CStr::from_ptr(&properties.extension_name as *const _) };
                c_str.to_str().expect("Invalid UTF-8 in extension name").to_owned()
            });

        Ok(extensions)
    }

    /// Returns a builder that may be used to create an instance
    pub fn builder() -> InstanceBuilder {
        InstanceBuilder {
            api_version: Version::VERSION_1_0,
            enable_extensions: vec![],
            enable_layers: vec![],
            app_name: None,
            app_version: None,
        }
    }

    /// Returns the version of the API the instance has been created with.
    pub fn version(&self) -> Version {
        self.0.version
    }

    /// Returns a raw handle to the underlying [`ash::Instance`].
    ///
    /// The returned handle may be used to access portions of the Vulkan API not in scope of the abstractions in this
    /// module.
    ///
    /// # Safety
    /// - The instance must not be destroyed.
    /// - The caller must guarantee usage of the handle and any objects created using the instance do not exceed the
    /// lifetime this instance.
    pub unsafe fn handle(&self) -> ash::Instance {
        self.0.instance.clone()
    }
}

pub(crate) struct InstanceInner {
    instance: ash::Instance,
    version: Version,
}

impl fmt::Debug for InstanceInner {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.debug_tuple("InstanceInner").field(&self.instance.handle()).finish()
    }
}

impl From<Arc<InstanceInner>> for Instance {
    fn from(inner: Arc<InstanceInner>) -> Self {
        Instance(inner)
    }
}

impl Drop for InstanceInner {
    fn drop(&mut self) {
        // SAFETY: Wrapping the inner instance in `Arc` ensures external synchronization per Vulkan specification.
        unsafe { self.instance.destroy_instance(None) };
    }
}
