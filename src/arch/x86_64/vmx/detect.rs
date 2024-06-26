use raw_cpuid::CpuId;

/// Checks if VT-x (vmx) is supported by our hardware.
pub fn has_hardware_support() -> bool {
    if let Some(feature) = CpuId::new().get_feature_info() {
        feature.has_vmx()
    } else {
        false
    }
}
