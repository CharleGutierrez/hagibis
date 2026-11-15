//! # Heterogeneous Hardware Discovery & Accelerator Introspection Engine
//!
//! Production-grade hardware scanner for Linux systems:
//! - DRM / DRI render and card nodes (`/dev/dri/renderD*`, `/dev/dri/card*`)
//! - Linux Accel subsystem (`/sys/class/accel/accel*`, `/dev/accel/accel*`)
//! - Dedicated NPU silicon (Intel VPU/IVPU, Rockchip RK3588, Google Coral Edge TPU, Qualcomm Hexagon)
//! - GPU compute drivers (Vulkan ICD manifests, OpenCL ICDs)
//! - Host CPU SIMD vector units (AVX-512, AVX2, FMA, ARM Neon)

use std::fs;
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};

use crate::error::{NpuRouterError, Result};

/// Known GPU vendors identified via PCI vendor IDs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuVendor {
    Intel,
    Nvidia,
    Amd,
    ArmMali,
    QualcommAdreno,
    Apple,
    Other,
}

impl GpuVendor {
    pub fn from_pci_id(id: u16) -> Self {
        match id {
            0x8086 => Self::Intel,
            0x10de => Self::Nvidia,
            0x1002 => Self::Amd,
            0x13b5 => Self::ArmMali,
            0x5143 => Self::QualcommAdreno,
            0x106b => Self::Apple,
            _ => Self::Other,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Intel => "Intel Corporation",
            Self::Nvidia => "NVIDIA Corporation",
            Self::Amd => "Advanced Micro Devices (AMD)",
            Self::ArmMali => "ARM Mali / Immortalis",
            Self::QualcommAdreno => "Qualcomm Adreno",
            Self::Apple => "Apple Silicon GPU",
            Self::Other => "Generic GPU Vendor",
        }
    }
}

/// Known NPU / TPU silicon vendors
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NpuVendor {
    IntelVpu,
    RockchipNpu,
    CoralTpu,
    QualcommHexagon,
    Hailo,
    AppleAne,
    GenericNpu,
}

impl NpuVendor {
    pub fn display_name(&self) -> &'static str {
        match self {
            Self::IntelVpu => "Intel AI Boost / NPU (IVPU)",
            Self::RockchipNpu => "Rockchip RK3588/RK3566 NPU",
            Self::CoralTpu => "Google Coral Edge TPU (Apex)",
            Self::QualcommHexagon => "Qualcomm Hexagon NPU / DSP",
            Self::Hailo => "Hailo-8 / Hailo-8L AI Processor",
            Self::AppleAne => "Apple Neural Engine (ANE)",
            Self::GenericNpu => "Generic Neural Processing Unit",
        }
    }
}

/// Hardware accelerator classification
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcceleratorType {
    DrmGpu(GpuVendor),
    LinuxAccelNpu(NpuVendor),
    RockchipNpu,
    CoralEdgeTpu,
    QualcommHexagonNpu,
    AppleAne,
    CpuSimd(SimdFeatureSet),
}

/// Compute features supported by the device
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComputeCapability {
    Fp32VectorSimd,
    Fp16HalfPrecision,
    Bf16BrainFloat,
    Int8Quantization,
    TensorCores,
    VulkanCompute,
    OpenCl,
    DirectMl,
    SharedMemoryZeroCopy,
}

/// CPU SIMD capabilities detected at runtime
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimdFeatureSet {
    pub has_avx512: bool,
    pub has_avx2: bool,
    pub has_fma: bool,
    pub has_neon: bool,
    pub vector_width_bits: usize,
    pub vector_lanes_f32: usize,
}

impl SimdFeatureSet {
    pub fn detect_host() -> Self {
        #[cfg(target_arch = "x86_64")]
        {
            let has_avx512 = is_x86_feature_detected!("avx512f");
            let has_avx2 = is_x86_feature_detected!("avx2");
            let has_fma = is_x86_feature_detected!("fma");
            let vector_width_bits = if has_avx512 {
                512
            } else if has_avx2 {
                256
            } else {
                128
            };
            Self {
                has_avx512,
                has_avx2,
                has_fma,
                has_neon: false,
                vector_width_bits,
                vector_lanes_f32: vector_width_bits / 32,
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self {
                has_avx512: false,
                has_avx2: false,
                has_fma: true,
                has_neon: true,
                vector_width_bits: 128,
                vector_lanes_f32: 4,
            }
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self {
                has_avx512: false,
                has_avx2: false,
                has_fma: false,
                has_neon: false,
                vector_width_bits: 64,
                vector_lanes_f32: 2,
            }
        }
    }
}

/// A discovered physical or virtual accelerator device
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceleratorDevice {
    pub id: String,
    pub name: String,
    pub device_type: AcceleratorType,
    pub device_node: Option<PathBuf>,
    pub sysfs_node: Option<PathBuf>,
    pub pci_vendor_id: Option<u16>,
    pub pci_device_id: Option<u16>,
    pub driver_name: Option<String>,
    pub vram_bytes: Option<u64>,
    pub capabilities: Vec<ComputeCapability>,
    pub estimated_tflops: f32,
    pub is_integrated: bool,
}

impl AcceleratorDevice {
    pub fn is_npu(&self) -> bool {
        matches!(
            self.device_type,
            AcceleratorType::LinuxAccelNpu(_)
                | AcceleratorType::RockchipNpu
                | AcceleratorType::CoralEdgeTpu
                | AcceleratorType::QualcommHexagonNpu
                | AcceleratorType::AppleAne
        )
    }

    pub fn is_gpu(&self) -> bool {
        matches!(self.device_type, AcceleratorType::DrmGpu(_))
    }
}

/// Complete host accelerator inventory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareInventory {
    pub devices: Vec<AcceleratorDevice>,
    pub cpu_simd: SimdFeatureSet,
    pub vulkan_icd_count: usize,
    pub opencl_icd_count: usize,
}

impl HardwareInventory {
    pub fn discover() -> Result<Self> {
        Self::discover_with_roots(
            Path::new("/sys"),
            Path::new("/dev"),
            Path::new("/etc"),
            Path::new("/usr"),
        )
    }

    pub fn discover_with_roots(
        sysfs_root: &Path,
        dev_root: &Path,
        etc_root: &Path,
        usr_root: &Path,
    ) -> Result<Self> {
        let mut devices = Vec::new();
        let cpu_simd = SimdFeatureSet::detect_host();

        discover_drm_devices(sysfs_root, dev_root, &mut devices);
        discover_accel_subsystem(sysfs_root, dev_root, &mut devices);
        discover_specialized_npus(sysfs_root, dev_root, &mut devices);

        let vulkan_icds = scan_vulkan_icds(etc_root, usr_root);
        let opencl_icds = scan_opencl_icds(etc_root);

        if !vulkan_icds.is_empty() {
            for dev in devices.iter_mut() {
                if dev.is_gpu() && !dev.capabilities.contains(&ComputeCapability::VulkanCompute) {
                    dev.capabilities.push(ComputeCapability::VulkanCompute);
                }
            }
        }

        let mut cpu_caps = vec![ComputeCapability::Fp32VectorSimd];
        if cpu_simd.has_fma {
            cpu_caps.push(ComputeCapability::Fp16HalfPrecision);
        }
        devices.push(AcceleratorDevice {
            id: "cpu:simd".to_string(),
            name: format!("Host CPU SIMD ({}b vector lanes)", cpu_simd.vector_width_bits),
            device_type: AcceleratorType::CpuSimd(cpu_simd),
            device_node: None,
            sysfs_node: None,
            pci_vendor_id: None,
            pci_device_id: None,
            driver_name: Some("kernel:cpu".to_string()),
            vram_bytes: None,
            capabilities: cpu_caps,
            estimated_tflops: estimate_cpu_simd_tflops(&cpu_simd),
            is_integrated: true,
        });

        Ok(Self {
            devices,
            cpu_simd,
            vulkan_icd_count: vulkan_icds.len(),
            opencl_icd_count: opencl_icds.len(),
        })
    }

    pub fn primary_accelerator(&self) -> Option<&AcceleratorDevice> {
        self.devices
            .iter()
            .filter(|d| !matches!(d.device_type, AcceleratorType::CpuSimd(_)))
            .max_by(|a, b| {
                let a_score = device_preference_score(a);
                let b_score = device_preference_score(b);
                a_score.cmp(&b_score)
            })
            .or_else(|| self.devices.iter().find(|d| matches!(d.device_type, AcceleratorType::CpuSimd(_))))
    }

    pub fn find_device(&self, id: &str) -> Option<&AcceleratorDevice> {
        self.devices.iter().find(|d| d.id == id)
    }

    pub fn has_npu(&self) -> bool {
        self.devices.iter().any(|d| d.is_npu())
    }

    pub fn has_gpu(&self) -> bool {
        self.devices.iter().any(|d| d.is_gpu())
    }

    pub fn render_summary(&self) -> String {
        let mut out = String::new();
        out.push_str("=== Heterogeneous Hardware Accelerator Inventory ===\n");
        out.push_str(&format!(
            "CPU SIMD: {} bits ({} f32 lanes) | AVX2: {} | AVX-512: {} | Neon: {}\n",
            self.cpu_simd.vector_width_bits,
            self.cpu_simd.vector_lanes_f32,
            self.cpu_simd.has_avx2,
            self.cpu_simd.has_avx512,
            self.cpu_simd.has_neon
        ));
        out.push_str(&format!(
            "Vulkan ICDs: {} | OpenCL ICDs: {}\n",
            self.vulkan_icd_count, self.opencl_icd_count
        ));
        out.push_str("Discovered Accelerators:\n");
        for dev in &self.devices {
            let kind = match &dev.device_type {
                AcceleratorType::DrmGpu(vendor) => format!("GPU ({})", vendor.display_name()),
                AcceleratorType::LinuxAccelNpu(vendor) => format!("Linux Accel NPU ({})", vendor.display_name()),
                AcceleratorType::RockchipNpu => "Rockchip NPU".to_string(),
                AcceleratorType::CoralEdgeTpu => "Google Coral TPU".to_string(),
                AcceleratorType::QualcommHexagonNpu => "Qualcomm Hexagon NPU".to_string(),
                AcceleratorType::AppleAne => "Apple Neural Engine".to_string(),
                AcceleratorType::CpuSimd(_) => "Host CPU SIMD".to_string(),
            };
            out.push_str(&format!(
                "  • [{}] {} | Type: {} | Driver: {} | Node: {}\n",
                dev.id,
                dev.name,
                kind,
                dev.driver_name.as_deref().unwrap_or("none"),
                dev.device_node.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "N/A".to_string())
            ));
        }
        out
    }
}

fn device_preference_score(device: &AcceleratorDevice) -> u32 {
    let mut score = 0;
    if device.is_npu() {
        score += 80;
    } else if device.is_gpu() {
        if !device.is_integrated {
            score += 100;
        } else {
            score += 60;
        }
    }
    if device.capabilities.contains(&ComputeCapability::TensorCores) {
        score += 20;
    }
    if device.capabilities.contains(&ComputeCapability::Int8Quantization) {
        score += 10;
    }
    score
}

fn estimate_cpu_simd_tflops(simd: &SimdFeatureSet) -> f32 {
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) as f32;
    let ghz = 3.2f32;
    let ops_per_clock = if simd.has_avx512 {
        32.0
    } else if simd.has_avx2 && simd.has_fma {
        16.0
    } else {
        8.0
    };
    (cores * ghz * ops_per_clock) / 1000.0
}

fn parse_hex_u16(s: &str) -> Option<u16> {
    let trimmed = s.trim().trim_start_matches("0x");
    u16::from_str_radix(trimmed, 16).ok()
}

fn discover_drm_devices(sysfs_root: &Path, dev_root: &Path, out: &mut Vec<AcceleratorDevice>) {
    let drm_sysfs = sysfs_root.join("class/drm");
    let dri_dev = dev_root.join("dri");

    if !drm_sysfs.is_dir() {
        let direct_node = dri_dev.join("renderD128");
        if direct_node.exists() {
            out.push(AcceleratorDevice {
                id: "drm:renderD128".to_string(),
                name: "Vulkan DRI Render Device".to_string(),
                device_type: AcceleratorType::DrmGpu(GpuVendor::Other),
                device_node: Some(direct_node),
                sysfs_node: None,
                pci_vendor_id: None,
                pci_device_id: None,
                driver_name: Some("drm".to_string()),
                vram_bytes: None,
                capabilities: vec![
                    ComputeCapability::Fp32VectorSimd,
                    ComputeCapability::VulkanCompute,
                ],
                estimated_tflops: 1.5,
                is_integrated: true,
            });
        }
        return;
    }

    let entries = match fs::read_dir(&drm_sysfs) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name_str = entry.file_name().to_string_lossy().to_string();
        if !name_str.starts_with("renderD") {
            continue;
        }

        let sysfs_path = entry.path();
        let dev_node = dri_dev.join(&name_str);

        let device_symlink = sysfs_path.join("device");
        let vendor_id = fs::read_to_string(device_symlink.join("vendor"))
            .ok()
            .and_then(|v| parse_hex_u16(&v));
        let device_id = fs::read_to_string(device_symlink.join("device"))
            .ok()
            .and_then(|d| parse_hex_u16(&d));

        let driver_name = fs::read_link(device_symlink.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

        let vendor = vendor_id.map(GpuVendor::from_pci_id).unwrap_or(GpuVendor::Other);
        let is_integrated = vendor != GpuVendor::Nvidia;

        let mut caps = vec![
            ComputeCapability::Fp32VectorSimd,
            ComputeCapability::VulkanCompute,
            ComputeCapability::SharedMemoryZeroCopy,
        ];
        if matches!(vendor, GpuVendor::Intel | GpuVendor::Nvidia | GpuVendor::Amd) {
            caps.push(ComputeCapability::Fp16HalfPrecision);
            caps.push(ComputeCapability::Int8Quantization);
        }
        if vendor == GpuVendor::Nvidia {
            caps.push(ComputeCapability::TensorCores);
        }

        out.push(AcceleratorDevice {
            id: format!("drm:{}", name_str),
            name: format!("{} DRM Device ({})", vendor.display_name(), name_str),
            device_type: AcceleratorType::DrmGpu(vendor),
            device_node: if dev_node.exists() { Some(dev_node) } else { None },
            sysfs_node: Some(sysfs_path),
            pci_vendor_id: vendor_id,
            pci_device_id: device_id,
            driver_name,
            vram_bytes: None,
            capabilities: caps,
            estimated_tflops: match vendor {
                GpuVendor::Nvidia => 8.5,
                GpuVendor::Amd => 6.0,
                GpuVendor::Intel => 2.2,
                _ => 1.0,
            },
            is_integrated,
        });
    }
}

fn discover_accel_subsystem(sysfs_root: &Path, dev_root: &Path, out: &mut Vec<AcceleratorDevice>) {
    let accel_sysfs = sysfs_root.join("class/accel");
    let accel_dev = dev_root.join("accel");

    if !accel_sysfs.is_dir() {
        return;
    }

    let entries = match fs::read_dir(&accel_sysfs) {
        Ok(e) => e,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let name_str = entry.file_name().to_string_lossy().to_string();
        if !name_str.starts_with("accel") {
            continue;
        }

        let sysfs_path = entry.path();
        let dev_node = accel_dev.join(&name_str);

        let device_symlink = sysfs_path.join("device");
        let vendor_id = fs::read_to_string(device_symlink.join("vendor"))
            .ok()
            .and_then(|v| parse_hex_u16(&v));
        let device_id = fs::read_to_string(device_symlink.join("device"))
            .ok()
            .and_then(|d| parse_hex_u16(&d));
        let driver_name = fs::read_link(device_symlink.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

        let npu_vendor = if vendor_id == Some(0x8086) || driver_name.as_deref() == Some("intel_vpu") {
            NpuVendor::IntelVpu
        } else {
            NpuVendor::GenericNpu
        };

        out.push(AcceleratorDevice {
            id: format!("accel:{}", name_str),
            name: format!("{} ({})", npu_vendor.display_name(), name_str),
            device_type: AcceleratorType::LinuxAccelNpu(npu_vendor),
            device_node: if dev_node.exists() { Some(dev_node) } else { None },
            sysfs_node: Some(sysfs_path),
            pci_vendor_id: vendor_id,
            pci_device_id: device_id,
            driver_name,
            vram_bytes: None,
            capabilities: vec![
                ComputeCapability::Fp16HalfPrecision,
                ComputeCapability::Int8Quantization,
                ComputeCapability::Bf16BrainFloat,
                ComputeCapability::SharedMemoryZeroCopy,
            ],
            estimated_tflops: 4.0,
            is_integrated: true,
        });
    }
}

fn discover_specialized_npus(sysfs_root: &Path, dev_root: &Path, out: &mut Vec<AcceleratorDevice>) {
    let rknpu_node = dev_root.join("rknpu");
    if rknpu_node.exists() {
        out.push(AcceleratorDevice {
            id: "npu:rk3588".to_string(),
            name: "Rockchip RK3588 6-TOPS Tri-Core NPU".to_string(),
            device_type: AcceleratorType::RockchipNpu,
            device_node: Some(rknpu_node),
            sysfs_node: None,
            pci_vendor_id: None,
            pci_device_id: None,
            driver_name: Some("rknpu".to_string()),
            vram_bytes: None,
            capabilities: vec![
                ComputeCapability::Int8Quantization,
                ComputeCapability::Fp16HalfPrecision,
                ComputeCapability::Bf16BrainFloat,
            ],
            estimated_tflops: 6.0,
            is_integrated: true,
        });
    }

    let apex_node = dev_root.join("apex_0");
    let apex_sysfs = sysfs_root.join("class/apex/apex_0");
    if apex_node.exists() || apex_sysfs.exists() {
        out.push(AcceleratorDevice {
            id: "tpu:coral".to_string(),
            name: "Google Coral Edge TPU (PCIe/USB 4-TOPS)".to_string(),
            device_type: AcceleratorType::CoralEdgeTpu,
            device_node: if apex_node.exists() { Some(apex_node) } else { None },
            sysfs_node: if apex_sysfs.exists() { Some(apex_sysfs) } else { None },
            pci_vendor_id: Some(0x1ac1),
            pci_device_id: Some(0x089a),
            driver_name: Some("apex".to_string()),
            vram_bytes: None,
            capabilities: vec![ComputeCapability::Int8Quantization],
            estimated_tflops: 4.0,
            is_integrated: false,
        });
    }

    let kgsl_node = dev_root.join("kgsl-3d0");
    let fastrpc_node = dev_root.join("fastrpc-cdsp");
    if kgsl_node.exists() || fastrpc_node.exists() {
        out.push(AcceleratorDevice {
            id: "npu:qualcomm_cdsp".to_string(),
            name: "Qualcomm Hexagon Compute DSP / NPU".to_string(),
            device_type: AcceleratorType::QualcommHexagonNpu,
            device_node: if fastrpc_node.exists() {
                Some(fastrpc_node)
            } else {
                Some(kgsl_node)
            },
            sysfs_node: None,
            pci_vendor_id: None,
            pci_device_id: None,
            driver_name: Some("fastrpc".to_string()),
            vram_bytes: None,
            capabilities: vec![
                ComputeCapability::Int8Quantization,
                ComputeCapability::Fp16HalfPrecision,
            ],
            estimated_tflops: 5.5,
            is_integrated: true,
        });
    }
}

fn scan_vulkan_icds(etc_root: &Path, usr_root: &Path) -> Vec<PathBuf> {
    let mut icds = Vec::new();
    let dirs = [
        etc_root.join("vulkan/icd.d"),
        usr_root.join("share/vulkan/icd.d"),
        usr_root.join("local/share/vulkan/icd.d"),
    ];

    for dir in &dirs {
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map(|e| e == "json").unwrap_or(false) {
                    icds.push(path);
                }
            }
        }
    }
    icds
}

fn scan_opencl_icds(etc_root: &Path) -> Vec<PathBuf> {
    let mut icds = Vec::new();
    let dir = etc_root.join("OpenCL/vendors");
    if let Ok(entries) = fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "icd").unwrap_or(false) {
                icds.push(path);
            }
        }
    }
    icds
}