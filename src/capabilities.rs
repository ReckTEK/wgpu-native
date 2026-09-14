use std::{ffi::CStr, ptr};

use crate::{conv, native, utils, EMPTY_STRING};

include!(concat!(env!("OUT_DIR"), "/proc_table.rs"));

#[no_mangle]
pub unsafe extern "C" fn wgpuGetProcAddress(name: native::WGPUStringView) -> native::WGPUProc {
    if name.data.is_null() || name.length == 0 {
        return None;
    }
    let bytes = if name.length == usize::MAX {
        CStr::from_ptr(name.data).to_bytes()
    } else {
        if name.length > isize::MAX as usize {
            return None;
        }
        std::slice::from_raw_parts(name.data.cast::<u8>(), name.length)
    };
    lookup_proc(bytes)
}

pub(crate) fn instance_features() -> Vec<native::WGPUInstanceFeatureName> {
    let mut features = Vec::with_capacity(2);
    if cfg!(feature = "spirv") {
        features.push(native::WGPUInstanceFeatureName_ShaderSourceSPIRV);
    }
    features.push(native::WGPUInstanceFeatureName_MultipleDevicesPerAdapter);
    features
}

#[no_mangle]
pub unsafe extern "C" fn wgpuGetInstanceFeatures(
    features: Option<&mut native::WGPUSupportedInstanceFeatures>,
) {
    let features = features.expect("invalid return pointer \"features\"");
    (features.features, features.featureCount) = owned_feature_list(instance_features());
}

#[no_mangle]
pub extern "C" fn wgpuHasInstanceFeature(
    feature: native::WGPUInstanceFeatureName,
) -> native::WGPUBool {
    match feature {
        native::WGPUInstanceFeatureName_ShaderSourceSPIRV => cfg!(feature = "spirv") as _,
        native::WGPUInstanceFeatureName_MultipleDevicesPerAdapter => 1,
        _ => 0,
    }
}

#[no_mangle]
pub unsafe extern "C" fn wgpuSupportedInstanceFeaturesFreeMembers(
    features: native::WGPUSupportedInstanceFeatures,
) {
    free_feature_list(features.features, features.featureCount);
}

#[cfg(feature = "wgsl")]
fn wgsl_language_features() -> impl Iterator<Item = native::WGPUWGSLLanguageFeatureName> {
    use wgc::naga::front::wgsl::ImplementedLanguageExtension;
    ImplementedLanguageExtension::all()
        .iter()
        .filter_map(|extension| match extension {
            ImplementedLanguageExtension::ReadOnlyAndReadWriteStorageTextures => {
                Some(native::WGPUWGSLLanguageFeatureName_ReadonlyAndReadwriteStorageTextures)
            }
            ImplementedLanguageExtension::Packed4x8IntegerDotProduct => {
                Some(native::WGPUWGSLLanguageFeatureName_Packed4x8IntegerDotProduct)
            }
            ImplementedLanguageExtension::PointerCompositeAccess => {
                Some(native::WGPUWGSLLanguageFeatureName_PointerCompositeAccess)
            }
            // The paired C header has no enum value for this language extension.
            ImplementedLanguageExtension::ImmediateAddressSpace => None,
        })
}

#[cfg(not(feature = "wgsl"))]
fn wgsl_language_features() -> impl Iterator<Item = native::WGPUWGSLLanguageFeatureName> {
    std::iter::empty()
}

#[no_mangle]
pub unsafe extern "C" fn wgpuInstanceGetWGSLLanguageFeatures(
    _instance: native::WGPUInstance,
    features: Option<&mut native::WGPUSupportedWGSLLanguageFeatures>,
) {
    let _instance_guard = crate::retain_handle(_instance);
    // The selected Naga frontend has the same language extensions for every instance.
    let features = features.expect("invalid return pointer \"features\"");
    (features.features, features.featureCount) =
        owned_feature_list(wgsl_language_features().collect());
}

#[no_mangle]
pub unsafe extern "C" fn wgpuInstanceHasWGSLLanguageFeature(
    _instance: native::WGPUInstance,
    feature: native::WGPUWGSLLanguageFeatureName,
) -> native::WGPUBool {
    let _instance_guard = crate::retain_handle(_instance);
    wgsl_language_features().any(|supported| supported == feature) as _
}

#[no_mangle]
pub unsafe extern "C" fn wgpuSupportedWGSLLanguageFeaturesFreeMembers(
    features: native::WGPUSupportedWGSLLanguageFeatures,
) {
    free_feature_list(features.features, features.featureCount);
}

fn owned_feature_list<T>(features: Vec<T>) -> (*const T, usize) {
    if features.is_empty() {
        return (ptr::null(), 0);
    }
    let features = features.into_boxed_slice();
    let count = features.len();
    (Box::into_raw(features).cast::<T>(), count)
}

unsafe fn free_feature_list<T>(features: *const T, count: usize) {
    if !features.is_null() {
        drop(Box::from_raw(ptr::slice_from_raw_parts_mut(
            features.cast_mut(),
            count,
        )));
    }
}

pub(crate) fn write_adapter_info(
    info: &mut native::WGPUAdapterInfo,
    adapter: wgt::AdapterInfo,
) -> native::WGPUStatus {
    if !info.nextInChain.is_null() {
        return native::WGPUStatus_Error;
    }

    info.vendor = utils::str_into_owned_string_view(&adapter.driver);
    info.architecture = EMPTY_STRING;
    info.device = utils::str_into_owned_string_view(&adapter.name);
    info.description = utils::str_into_owned_string_view(&adapter.driver_info);
    info.backendType = conv::map_backend_type(adapter.backend);
    info.adapterType = conv::map_adapter_type(adapter.device_type);
    info.vendorID = adapter.vendor;
    info.deviceID = adapter.device;
    info.subgroupMinSize = adapter.subgroup_min_size;
    info.subgroupMaxSize = adapter.subgroup_max_size;
    native::WGPUStatus_Success
}

#[no_mangle]
pub unsafe extern "C" fn wgpuDeviceGetAdapterInfo(
    device: native::WGPUDevice,
    info: Option<&mut native::WGPUAdapterInfo>,
) -> native::WGPUStatus {
    let _device_guard = crate::retain_handle(device);
    let device = device.as_ref().expect("invalid device");
    let info = info.expect("invalid return pointer \"info\"");
    write_adapter_info(info, device.inner.clone().adapter_info())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(bytes: &[u8]) -> native::WGPUStringView {
        native::WGPUStringView {
            data: bytes.as_ptr().cast(),
            length: bytes.len(),
        }
    }

    #[test]
    fn procedure_lookup_obeys_string_view_boundaries() {
        unsafe {
            let name = b"wgpuGetVersionextra";
            let mut bounded = view(name);
            bounded.length = b"wgpuGetVersion".len();
            let proc = wgpuGetProcAddress(bounded).expect("known function");
            let version: unsafe extern "C" fn() -> u32 = std::mem::transmute(proc);
            assert_eq!(version(), crate::logging::wgpuGetVersion());
            assert!(wgpuGetProcAddress(view(name)).is_none());

            let mut terminated = view(b"wgpuGetVersion\0ignored");
            terminated.length = usize::MAX;
            assert!(wgpuGetProcAddress(terminated).is_some());
            assert!(wgpuGetProcAddress(view(b"wgpuGetVersion\0")).is_none());
            assert!(wgpuGetProcAddress(view(b"wgpuMissingFunction")).is_none());
            assert!(wgpuGetProcAddress(view(b"wgpugetversion")).is_none());
            assert!(wgpuGetProcAddress(view(b"wgpu\xff")).is_none());
            assert!(wgpuGetProcAddress(view("wgpu\u{e9}".as_bytes())).is_none());
            assert!(wgpuGetProcAddress(view(b"")).is_none());
            for length in [0, 1, usize::MAX] {
                assert!(wgpuGetProcAddress(native::WGPUStringView {
                    data: ptr::null(),
                    length,
                })
                .is_none());
            }
        }
    }

    #[test]
    fn every_target_header_procedure_has_an_address() {
        for name in PROC_NAMES {
            assert!(unsafe { wgpuGetProcAddress(view(name)) }.is_some());
        }
        assert!(PROC_NAMES.contains(&b"wgpuGetProcAddress".as_slice()));
        assert!(PROC_NAMES.contains(&b"wgpuSetLogCallback".as_slice()));
        assert!(PROC_NAMES.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn instance_feature_list_matches_supported_build_paths() {
        let mut features = native::WGPUSupportedInstanceFeatures {
            features: ptr::null(),
            featureCount: 0,
        };
        unsafe { wgpuGetInstanceFeatures(Some(&mut features)) };
        let reported =
            unsafe { std::slice::from_raw_parts(features.features, features.featureCount) };
        assert!(reported.contains(&native::WGPUInstanceFeatureName_MultipleDevicesPerAdapter));
        assert_eq!(
            reported.contains(&native::WGPUInstanceFeatureName_ShaderSourceSPIRV),
            cfg!(feature = "spirv")
        );
        for feature in reported {
            assert_eq!(wgpuHasInstanceFeature(*feature), 1);
        }
        assert_eq!(
            wgpuHasInstanceFeature(native::WGPUInstanceFeatureName_TimedWaitAny),
            0
        );
        assert_eq!(wgpuHasInstanceFeature(u32::MAX), 0);
        unsafe { wgpuSupportedInstanceFeaturesFreeMembers(features) };
    }

    #[test]
    fn wgsl_features_match_the_actual_frontend() {
        let instance = unsafe { crate::wgpuCreateInstance(None) };
        let mut features = native::WGPUSupportedWGSLLanguageFeatures {
            features: ptr::null(),
            featureCount: 0,
        };
        unsafe { wgpuInstanceGetWGSLLanguageFeatures(instance, Some(&mut features)) };
        let reported = utils::make_slice(features.features, features.featureCount);
        assert_eq!(reported, wgsl_language_features().collect::<Vec<_>>());
        for feature in reported {
            assert_eq!(
                unsafe { wgpuInstanceHasWGSLLanguageFeature(instance, *feature) },
                1
            );
        }
        assert_eq!(
            unsafe {
                wgpuInstanceHasWGSLLanguageFeature(
                    instance,
                    native::WGPUWGSLLanguageFeatureName_UnrestrictedPointerParameters,
                )
            },
            0
        );
        #[cfg(feature = "wgsl")]
        for extension in wgc::naga::front::wgsl::ImplementedLanguageExtension::all() {
            let source = format!(
                "requires {}; @compute @workgroup_size(1) fn main() {{}}",
                extension.to_ident()
            );
            wgc::naga::front::wgsl::parse_str(&source)
                .expect("advertised language extension parses");
        }
        unsafe {
            wgpuSupportedWGSLLanguageFeaturesFreeMembers(features);
            crate::wgpuInstanceRelease(instance);
        }
    }

    #[test]
    fn adapter_info_preserves_real_metadata_and_owned_strings() {
        let source = wgt::AdapterInfo {
            name: "test adapter".into(),
            vendor: 0x1234,
            device: 0x5678,
            device_type: wgt::DeviceType::DiscreteGpu,
            device_pci_bus_id: String::new(),
            driver: "test driver".into(),
            driver_info: "driver version".into(),
            backend: wgt::Backend::Vulkan,
            subgroup_min_size: 32,
            subgroup_max_size: 64,
            transient_saves_memory: Some(false),
            limit_bucket: None,
        };
        let mut info: native::WGPUAdapterInfo = unsafe { std::mem::zeroed() };
        assert_eq!(
            write_adapter_info(&mut info, source.clone()),
            native::WGPUStatus_Success
        );
        unsafe {
            assert_eq!(
                utils::string_view_into_str(info.device),
                Some("test adapter")
            );
            assert_eq!(
                utils::string_view_into_str(info.vendor),
                Some("test driver")
            );
            assert_eq!(
                utils::string_view_into_str(info.description),
                Some("driver version")
            );
            assert_eq!(info.vendorID, 0x1234);
            assert_eq!(info.deviceID, 0x5678);
            assert_eq!(info.subgroupMinSize, 32);
            assert_eq!(info.subgroupMaxSize, 64);
            assert_eq!(info.backendType, native::WGPUBackendType_Vulkan);
            assert_eq!(info.adapterType, native::WGPUAdapterType_DiscreteGPU);
            crate::wgpuAdapterInfoFreeMembers(info);
        }

        let mut chain = native::WGPUChainedStruct {
            next: ptr::null_mut(),
            sType: u32::MAX,
        };
        let mut rejected: native::WGPUAdapterInfo = unsafe { std::mem::zeroed() };
        rejected.nextInChain = &mut chain;
        assert_eq!(
            write_adapter_info(&mut rejected, source),
            native::WGPUStatus_Error
        );
        assert!(rejected.device.data.is_null());
    }
}
