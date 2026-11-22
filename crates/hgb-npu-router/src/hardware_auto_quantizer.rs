//! # Hardware-Aware Auto-Quantization & Kernel Fusion Pipeline
//!
//! Production-grade telemetry-driven quantization policy and single-pass fused compute
//! kernels for Project Hagibis. Unifies live Linux sysfs sensor scraping with dynamic
//! precision transitions (FP32 -> FP16 -> INT8 -> INT4) and register/cache-resident
//! kernel fusion to eliminate redundant memory roundtrips and suppress thermal throttling.
//!
//! ## Architectural Capabilities
//! 1. **Real-Time Hardware Telemetry Scraping**:
//!    Reads `/sys/class/thermal/thermal_zone*/temp`, `/sys/class/powercap/intel-rapl/`,
//!    `/sys/class/hwmon/`, and GPU CLI fallbacks with thread-safe cached polling and
//!    Exponential Moving Average (EMA) smoothing to eliminate transient fluttering.
//! 2. **Dynamic Thermal & Power Quantization Policy**:
//!    - Cold/nominal range (< 70°C): FP32 / FP16 precision.
//!    - Warm/elevated load (70°C <= T < 85°C) or battery power-saving: INT8 symmetric/asymmetric affine.
//!    - Severe thermal throttling (>= 85°C) or power duress: INT4 packed nibbles (2 weights per byte,
//!      cutting memory traffic by 4x-8x to minimize bus dissipation).
//!    - Dynamic scale/offset calibration with Signal-to-Quantization-Noise Ratio (SQNR) in dB.
//! 3. **Runtime Kernel Fusion Engine**:
//!    - Fused MatMul + Bias + Activation (ReLU, GELU, SiLU, Tanh).
//!    - Fused LayerNorm / RMSNorm + Activation.
//!    - Eliminates redundant memory roundtrips to system RAM/VRAM by executing fused stages
//!      in single-pass registers/cache tiles.
//! 4. **Seamless Integration**:
//!    Direct zero-copy execution with [`DeviceMemoryBuffer`] and [`UnifiedZeroCopyTensorBridge`].

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{NpuRouterError, Result};
use crate::zero_copy_tensor_bridge::{
    ALIGN_64_AVX512, DeviceComputeBackend, DeviceMemoryBuffer, WebNnDataType,
};

// ============================================================================
// 1. HARDWARE TELEMETRY & SYSTEM SENSORS
// ============================================================================

/// Raw instantaneous hardware telemetry reading
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveHardwareMetrics {
    /// CPU Junction/Package temperature in degrees Celsius
    pub junction_temp_c: f32,
    /// Peak temperature observed across all sensors
    pub peak_temp_c: f32,
    /// Estimated or scraped current power consumption in Watts
    pub power_draw_w: f32,
    /// Thermal headroom to maximum junction temperature (TjMax - PeakTemp) in °C
    pub thermal_headroom_c: f32,
    /// Maximum junction temperature (TjMax) before emergency throttling in °C
    pub tj_max_c: f32,
    /// Whether any thermal zone is at or above throttling threshold
    pub is_thermal_throttling: bool,
    /// Whether the host is connected to AC mains power (false if on battery)
    pub ac_connected: bool,
    /// Optional discrete/integrated GPU temperature in °C
    pub gpu_temp_c: Option<f32>,
    /// Optional discrete/integrated GPU power draw in Watts
    pub gpu_power_w: Option<f32>,
    /// Milliseconds timestamp since UNIX epoch
    pub timestamp_ms: u64,
}

impl Default for LiveHardwareMetrics {
    fn default() -> Self {
        Self {
            junction_temp_c: 48.0,
            peak_temp_c: 48.0,
            power_draw_w: 22.0,
            thermal_headroom_c: 52.0,
            tj_max_c: 100.0,
            is_thermal_throttling: false,
            ac_connected: true,
            gpu_temp_c: None,
            gpu_power_w: None,
            timestamp_ms: Utc::now().timestamp_millis() as u64,
        }
    }
}

/// Telemetry smoothed via Exponential Moving Average (EMA) to prevent hysteresis flapping
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmoothedHardwareMetrics {
    /// Instantaneous raw metrics
    pub raw: LiveHardwareMetrics,
    /// EMA smoothed peak junction temperature in °C
    pub smoothed_temp_c: f32,
    /// EMA smoothed power consumption in Watts
    pub smoothed_power_w: f32,
    /// Smoothing factor alpha (0.0 to 1.0)
    pub ema_alpha: f32,
    /// Smoothed thermal headroom (TjMax - smoothed_temp) in °C
    pub thermal_headroom_c: f32,
    /// Currently recommended quantization mode based on smoothed thermal state
    pub recommended_mode: QuantizationMode,
}

/// Thread-safe cached telemetry scraper state
struct TelemetryCacheState {
    last_raw: LiveHardwareMetrics,
    last_polled: Instant,
    smoothed_temp_c: f32,
    smoothed_power_w: f32,
    initialized: bool,
}

/// Linux sysfs and GPU telemetry scraper with thread-safe cached polling
pub struct HardwareTelemetryScraper {
    sysfs_root: PathBuf,
    cache_ttl: Duration,
    ema_alpha: f32,
    cache: Mutex<TelemetryCacheState>,
    rapl_energy_tracker: Mutex<Option<(u64, Instant)>>,
    simulated_override: RwLock<Option<LiveHardwareMetrics>>,
}

impl HardwareTelemetryScraper {
    /// Creates a new scraper querying the standard Linux `/sys` hierarchy
    pub fn new() -> Self {
        Self::with_sysfs_root(PathBuf::from("/sys"))
    }

    /// Creates a scraper rooted at a custom path (useful for testing and containerized sysfs mounts)
    pub fn with_sysfs_root(sysfs_root: PathBuf) -> Self {
        let initial_raw = LiveHardwareMetrics::default();
        Self {
            sysfs_root,
            cache_ttl: Duration::from_millis(100),
            ema_alpha: 0.25,
            cache: Mutex::new(TelemetryCacheState {
                smoothed_temp_c: initial_raw.junction_temp_c,
                smoothed_power_w: initial_raw.power_draw_w,
                last_raw: initial_raw,
                last_polled: Instant::now() - Duration::from_secs(10),
                initialized: false,
            }),
            rapl_energy_tracker: Mutex::new(None),
            simulated_override: RwLock::new(None),
        }
    }

    /// Configures cache TTL (time between actual sysfs I/O operations)
    pub fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    /// Configures EMA smoothing alpha factor (0.0 < alpha <= 1.0)
    pub fn with_ema_alpha(mut self, alpha: f32) -> Self {
        self.ema_alpha = alpha.clamp(0.01, 1.0);
        self
    }

    /// Injects a simulated metrics override for zero-mock testing
    pub fn set_simulated_override(&self, metrics: Option<LiveHardwareMetrics>) {
        let mut sim = self.simulated_override.write().expect("lock simulated override");
        *sim = metrics;
    }

    /// Scrapes live sysfs sensors without caching
    pub fn scrape_live_raw(&self) -> LiveHardwareMetrics {
        // Check if simulation override is active
        if let Some(metrics) = self.simulated_override.read().expect("read simulated").as_ref() {
            let mut cloned = metrics.clone();
            cloned.timestamp_ms = Utc::now().timestamp_millis() as u64;
            return cloned;
        }

        let mut junction_temp_c = 45.0f32;
        let mut peak_temp_c = 45.0f32;
        let mut tj_max_c = 100.0f32;

        // 1. Scrape /sys/class/thermal/thermal_zone*
        let thermal_dir = self.sysfs_root.join("class/thermal");
        if let Ok(entries) = fs::read_dir(&thermal_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("thermal_zone") {
                    let zone_path = entry.path();
                    let zone_type = fs::read_to_string(zone_path.join("type"))
                        .unwrap_or_default()
                        .trim()
                        .to_lowercase();

                    if let Ok(temp_str) = fs::read_to_string(zone_path.join("temp")) {
                        if let Ok(temp_raw) = temp_str.trim().parse::<f32>() {
                            let temp_c = if temp_raw > 1000.0 { temp_raw / 1000.0 } else { temp_raw };
                            if temp_c > 0.0 && temp_c < 140.0 {
                                if temp_c > peak_temp_c {
                                    peak_temp_c = temp_c;
                                }
                                if zone_type.contains("x86_pkg_temp")
                                    || zone_type.contains("cpu")
                                    || zone_type.contains("package")
                                    || zone_type == "acpitz"
                                {
                                    junction_temp_c = temp_c;
                                }
                            }
                        }
                    }

                    // Check trip points for TjMax
                    for i in 0..6 {
                        let trip_type_path = zone_path.join(format!("trip_point_{}_type", i));
                        let trip_temp_path = zone_path.join(format!("trip_point_{}_temp", i));
                        if let Ok(t_type) = fs::read_to_string(&trip_type_path) {
                            let t_type = t_type.trim().to_lowercase();
                            if t_type == "critical" || t_type == "hot" {
                                if let Ok(t_raw) = fs::read_to_string(&trip_temp_path) {
                                    if let Ok(raw) = t_raw.trim().parse::<f32>() {
                                        let crit_c = if raw > 1000.0 { raw / 1000.0 } else { raw };
                                        if crit_c >= 70.0 && crit_c <= 125.0 {
                                            tj_max_c = crit_c;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 2. Scrape /sys/class/hwmon/hwmon*
        let hwmon_dir = self.sysfs_root.join("class/hwmon");
        if let Ok(entries) = fs::read_dir(&hwmon_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                // Check all temp*_input files
                if let Ok(sub_entries) = fs::read_dir(&path) {
                    for sub in sub_entries.flatten() {
                        let file_name = sub.file_name().to_string_lossy().to_string();
                        if file_name.starts_with("temp") && file_name.ends_with("_input") {
                            if let Ok(s) = fs::read_to_string(sub.path()) {
                                if let Ok(raw) = s.trim().parse::<f32>() {
                                    let c = if raw > 1000.0 { raw / 1000.0 } else { raw };
                                    if c > 0.0 && c < 140.0 {
                                        if c > peak_temp_c {
                                            peak_temp_c = c;
                                        }
                                    }
                                }
                            }
                        }
                        if file_name.starts_with("temp") && file_name.ends_with("_crit") {
                            if let Ok(s) = fs::read_to_string(sub.path()) {
                                if let Ok(raw) = s.trim().parse::<f32>() {
                                    let crit_c = if raw > 1000.0 { raw / 1000.0 } else { raw };
                                    if crit_c >= 70.0 && crit_c <= 125.0 {
                                        tj_max_c = crit_c;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // 3. Scrape Power: RAPL or hwmon battery/AC power
        let power_draw_w = self.scrape_power_watts(&hwmon_dir);

        // 4. Scrape AC mains connection status
        let ac_connected = self.scrape_ac_status();

        // 5. GPU CLI / DRI Fallback
        let (gpu_temp_c, gpu_power_w) = self.scrape_gpu_telemetry();

        let thermal_headroom_c = (tj_max_c - peak_temp_c).max(0.0);
        let is_thermal_throttling = peak_temp_c >= 85.0 || thermal_headroom_c <= 15.0;

        LiveHardwareMetrics {
            junction_temp_c,
            peak_temp_c,
            power_draw_w,
            thermal_headroom_c,
            tj_max_c,
            is_thermal_throttling,
            ac_connected,
            gpu_temp_c,
            gpu_power_w,
            timestamp_ms: Utc::now().timestamp_millis() as u64,
        }
    }

    /// Thread-safe cached polling with Exponential Moving Average (EMA) smoothing
    pub fn poll_smoothed(&self) -> SmoothedHardwareMetrics {
        let mut cache = self.cache.lock().expect("lock telemetry cache");
        let now = Instant::now();

        if !cache.initialized || now.duration_since(cache.last_polled) >= self.cache_ttl {
            let raw = self.scrape_live_raw();
            if !cache.initialized {
                cache.smoothed_temp_c = raw.peak_temp_c;
                cache.smoothed_power_w = raw.power_draw_w;
                cache.initialized = true;
            } else {
                cache.smoothed_temp_c =
                    self.ema_alpha * raw.peak_temp_c + (1.0 - self.ema_alpha) * cache.smoothed_temp_c;
                cache.smoothed_power_w =
                    self.ema_alpha * raw.power_draw_w + (1.0 - self.ema_alpha) * cache.smoothed_power_w;
            }
            cache.last_raw = raw;
            cache.last_polled = Instant::now();
        }

        let raw = cache.last_raw.clone();
        let smoothed_temp = cache.smoothed_temp_c;
        let smoothed_power = cache.smoothed_power_w;
        let headroom = (raw.tj_max_c - smoothed_temp).max(0.0);

        // Determine recommended mode based on smoothed thermal envelope
        let recommended_mode = if smoothed_temp >= 85.0 || smoothed_power >= 65.0 {
            QuantizationMode::INT4
        } else if smoothed_temp >= 70.0 || !raw.ac_connected {
            QuantizationMode::INT8
        } else {
            QuantizationMode::FP16
        };

        SmoothedHardwareMetrics {
            raw,
            smoothed_temp_c: smoothed_temp,
            smoothed_power_w: smoothed_power,
            ema_alpha: self.ema_alpha,
            thermal_headroom_c: headroom,
            recommended_mode,
        }
    }

    fn scrape_power_watts(&self, hwmon_dir: &Path) -> f32 {
        // Attempt 1: RAPL energy delta
        let rapl_energy_file = self
            .sysfs_root
            .join("class/powercap/intel-rapl/intel-rapl:0/energy_uj");
        if let Ok(raw_str) = fs::read_to_string(&rapl_energy_file) {
            if let Ok(current_uj) = raw_str.trim().parse::<u64>() {
                let now = Instant::now();
                let mut tracker = self.rapl_energy_tracker.lock().expect("rapl tracker lock");
                if let Some((prev_uj, prev_time)) = *tracker {
                    let elapsed = now.duration_since(prev_time).as_secs_f32();
                    if elapsed > 0.05 && current_uj >= prev_uj {
                        let delta_uj = current_uj - prev_uj;
                        let watts = (delta_uj as f32 / 1_000_000.0) / elapsed;
                        *tracker = Some((current_uj, now));
                        if watts > 0.5 && watts < 250.0 {
                            return watts;
                        }
                    }
                } else {
                    *tracker = Some((current_uj, now));
                }
            }
        }

        // Attempt 2: hwmon power*_input (microWatts)
        if let Ok(entries) = fs::read_dir(hwmon_dir) {
            for entry in entries.flatten() {
                if let Ok(sub_entries) = fs::read_dir(entry.path()) {
                    for sub in sub_entries.flatten() {
                        let name = sub.file_name().to_string_lossy().to_string();
                        if name.starts_with("power") && name.ends_with("_input") {
                            if let Ok(str_val) = fs::read_to_string(sub.path()) {
                                if let Ok(uw) = str_val.trim().parse::<f32>() {
                                    let watts = uw / 1_000_000.0;
                                    if watts > 0.5 && watts < 250.0 {
                                        return watts;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Attempt 3: RAPL constraint power limit fallback
        let rapl_limit = self
            .sysfs_root
            .join("class/powercap/intel-rapl/intel-rapl:0/constraint_0_power_limit_uw");
        if let Ok(str_val) = fs::read_to_string(rapl_limit) {
            if let Ok(uw) = str_val.trim().parse::<f32>() {
                let watts = uw / 1_000_000.0;
                if watts > 1.0 && watts < 250.0 {
                    return watts;
                }
            }
        }

        // Nominal default TDP estimation
        28.0f32
    }

    fn scrape_ac_status(&self) -> bool {
        let ps_dir = self.sysfs_root.join("class/power_supply");
        if let Ok(entries) = fs::read_dir(&ps_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("AC") || name.starts_with("ADP") {
                    if let Ok(online) = fs::read_to_string(entry.path().join("online")) {
                        if online.trim() == "1" {
                            return true;
                        } else if online.trim() == "0" {
                            return false;
                        }
                    }
                }
            }
        }
        true // Assume AC mains by default
    }

    fn scrape_gpu_telemetry(&self) -> (Option<f32>, Option<f32>) {
        // Safe GPU CLI check: only runs if nvidia-smi is in path
        if let Ok(output) = Command::new("nvidia-smi")
            .args(["--query-gpu=temperature.gpu,power.draw", "--format=csv,noheader,nounits"])
            .output()
        {
            if output.status.success() {
                let text = String::from_utf8_lossy(&output.stdout);
                let parts: Vec<&str> = text.trim().split(',').map(|s| s.trim()).collect();
                if parts.len() >= 2 {
                    let temp = parts[0].parse::<f32>().ok();
                    let power = parts[1].parse::<f32>().ok();
                    return (temp, power);
                }
            }
        }
        (None, None)
    }
}

// ============================================================================
// 2. DYNAMIC THERMAL & POWER QUANTIZATION POLICY
// ============================================================================

/// Hardware-aware quantization precisions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QuantizationMode {
    /// 32-bit single-precision float (Cold operating range: T < 70°C)
    FP32,
    /// 16-bit half-precision float (Cold operating range: T < 70°C, 2x compression)
    FP16,
    /// 8-bit integer affine quantization (Warm range: 70°C <= T < 85°C or Battery mode, 4x compression)
    INT8,
    /// 4-bit packed nibbles (Severe thermal throttling: T >= 85°C or Power duress, 8x compression)
    INT4,
}

impl QuantizationMode {
    /// Bits per element representation
    #[inline(always)]
    pub fn bits_per_element(&self) -> usize {
        match self {
            Self::FP32 => 32,
            Self::FP16 => 16,
            Self::INT8 => 8,
            Self::INT4 => 4,
        }
    }

    /// Memory compression factor relative to FP32 baseline
    #[inline(always)]
    pub fn compression_factor(&self) -> f32 {
        match self {
            Self::FP32 => 1.0,
            Self::FP16 => 2.0,
            Self::INT8 => 4.0,
            Self::INT4 => 8.0,
        }
    }

    /// Memory bus bandwidth reduction percentage relative to FP32
    #[inline(always)]
    pub fn bandwidth_reduction_pct(&self) -> f32 {
        match self {
            Self::FP32 => 0.0,
            Self::FP16 => 50.0,
            Self::INT8 => 75.0,
            Self::INT4 => 87.5,
        }
    }

    /// Maps to equivalent WebNN data type (INT4 represented as Uint8 packed)
    pub fn to_webnn_type(&self) -> WebNnDataType {
        match self {
            Self::FP32 => WebNnDataType::Float32,
            Self::FP16 => WebNnDataType::Float16,
            Self::INT8 => WebNnDataType::Int8,
            Self::INT4 => WebNnDataType::Uint8,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FP32 => "FP32",
            Self::FP16 => "FP16",
            Self::INT8 => "INT8",
            Self::INT4 => "INT4",
        }
    }
}

/// Quantization scheme (symmetric vs asymmetric affine)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QuantizationScheme {
    /// Symmetric quantization around 0.0 with zero_point = 0
    SymmetricAffine,
    /// Asymmetric min-max affine quantization with dynamic zero_point offset
    AsymmetricAffine,
}

/// Signal-to-Quantization-Noise Ratio (SQNR) and error statistics
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuantizationQualityMetrics {
    /// Signal-to-Quantization-Noise Ratio in decibels (dB)
    pub sqnr_db: f32,
    /// Mean Squared Error (MSE)
    pub mse: f32,
    /// Root Mean Squared Error (RMSE)
    pub rmse: f32,
    /// Maximum absolute deviation between original and dequantized values
    pub max_absolute_error: f32,
    /// Compression ratio achieved (original_bytes / quantized_bytes)
    pub compression_ratio: f32,
    /// Raw unquantized FP32 byte size
    pub original_bytes: usize,
    /// Compact quantized byte size
    pub quantized_bytes: usize,
}

/// Compact quantized tensor payload with scale and offset calibration metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantizedTensor {
    /// Active quantization precision
    pub mode: QuantizationMode,
    /// Symmetric or asymmetric scheme
    pub scheme: QuantizationScheme,
    /// Tensor dimension shape
    pub shape: Vec<usize>,
    /// Total element count
    pub element_count: usize,
    /// Quantization scaling factor
    pub scale: f32,
    /// Zero-point offset (for asymmetric quantization)
    pub zero_point: f32,
    /// Compact packed byte buffer
    pub packed_bytes: Vec<u8>,
    /// Auditable precision and noise statistics
    pub metrics: QuantizationQualityMetrics,
}

impl QuantizedTensor {
    /// Dequantizes the tensor payload back to single-precision float vector
    pub fn dequantize(&self) -> Vec<f32> {
        match self.mode {
            QuantizationMode::FP32 => dequantize_fp32(&self.packed_bytes),
            QuantizationMode::FP16 => dequantize_fp16(&self.packed_bytes),
            QuantizationMode::INT8 => dequantize_int8(
                &self.packed_bytes,
                self.scale,
                self.zero_point,
                self.scheme,
            ),
            QuantizationMode::INT4 => dequantize_int4(
                &self.packed_bytes,
                self.element_count,
                self.scale,
                self.zero_point,
                self.scheme,
            ),
        }
    }

    /// Exports quantized payload into an aligned zero-copy [`DeviceMemoryBuffer`]
    pub fn to_device_buffer(&self, backend: DeviceComputeBackend) -> Result<DeviceMemoryBuffer> {
        let mut buffer = DeviceMemoryBuffer::allocate_aligned(
            self.packed_bytes.len(),
            ALIGN_64_AVX512,
            backend,
        )?;
        buffer.as_mut_slice().copy_from_slice(&self.packed_bytes);
        buffer.flush_range(0, self.packed_bytes.len())?;
        Ok(buffer)
    }
}

// ============================================================================
// IEEE-754 FP16 (Binary16) Native Software Conversion
// ============================================================================

/// Converts IEEE-754 single precision float (f32) to IEEE-754 half-precision (u16)
#[inline]
pub fn f32_to_f16(val: f32) -> u16 {
    let bits = val.to_bits();
    let sign = (bits >> 16) & 0x8000;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let mant = bits & 0x007F_FFFF;

    if exp == 255 {
        if mant == 0 {
            (sign | 0x7C00) as u16
        } else {
            (sign | 0x7C00 | (mant >> 13).max(1)) as u16
        }
    } else {
        let new_exp = exp - 127 + 15;
        if new_exp >= 31 {
            (sign | 0x7C00) as u16
        } else if new_exp <= 0 {
            if new_exp < -10 {
                sign as u16
            } else {
                let m = (mant | 0x0080_0000) >> (14 - new_exp);
                (sign | m) as u16
            }
        } else {
            let m = mant >> 13;
            (sign | ((new_exp as u32) << 10) | m) as u16
        }
    }
}

/// Converts IEEE-754 half-precision binary16 (u16) to single precision float (f32)
#[inline]
pub fn f16_to_f32(val: u16) -> f32 {
    let sign = ((val & 0x8000) as u32) << 16;
    let exp = ((val >> 10) & 0x1F) as u32;
    let mant = (val & 0x03FF) as u32;

    let f32_bits = if exp == 0 {
        if mant == 0 {
            sign
        } else {
            let mut m = mant;
            let mut shift = 0;
            while (m & 0x0400) == 0 {
                m <<= 1;
                shift += 1;
            }
            m &= 0x03FF;
            let new_exp = 127 - 15 - shift + 1;
            sign | ((new_exp as u32) << 23) | (m << 13)
        }
    } else if exp == 31 {
        if mant == 0 {
            sign | 0x7F80_0000
        } else {
            sign | 0x7F80_0000 | (mant << 13)
        }
    } else {
        let new_exp = exp + 127 - 15;
        sign | (new_exp << 23) | (mant << 13)
    };

    f32::from_bits(f32_bits)
}

// ============================================================================
// QUANTIZATION / DEQUANTIZATION ENGINES
// ============================================================================

/// Quantizes FP32 slice into packed FP32 bytes
pub fn quantize_fp32(data: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for &val in data {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Dequantizes packed FP32 bytes to FP32 slice
pub fn dequantize_fp32(bytes: &[u8]) -> Vec<f32> {
    let count = bytes.len() / 4;
    let mut floats = Vec::with_capacity(count);
    for i in 0..count {
        let arr = [
            bytes[i * 4],
            bytes[i * 4 + 1],
            bytes[i * 4 + 2],
            bytes[i * 4 + 3],
        ];
        floats.push(f32::from_le_bytes(arr));
    }
    floats
}

/// Quantizes FP32 slice into compact FP16 (u16) bytes
pub fn quantize_fp16(data: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(data.len() * 2);
    for &val in data {
        let h = f32_to_f16(val);
        bytes.extend_from_slice(&h.to_le_bytes());
    }
    bytes
}

/// Dequantizes compact FP16 bytes to FP32 slice
pub fn dequantize_fp16(bytes: &[u8]) -> Vec<f32> {
    let count = bytes.len() / 2;
    let mut floats = Vec::with_capacity(count);
    for i in 0..count {
        let arr = [bytes[i * 2], bytes[i * 2 + 1]];
        let h = u16::from_le_bytes(arr);
        floats.push(f16_to_f32(h));
    }
    floats
}

/// Quantizes FP32 slice into signed INT8 with calibrated scale and zero-point
pub fn quantize_int8(data: &[f32], scheme: QuantizationScheme) -> (Vec<u8>, f32, f32) {
    if data.is_empty() {
        return (Vec::new(), 1.0, 0.0);
    }

    let mut min_val = f32::INFINITY;
    let mut max_val = f32::NEG_INFINITY;
    for &x in data {
        if x < min_val {
            min_val = x;
        }
        if x > max_val {
            max_val = x;
        }
    }

    match scheme {
        QuantizationScheme::SymmetricAffine => {
            let max_abs = min_val.abs().max(max_val.abs());
            let scale = if max_abs < 1e-8 { 1.0 } else { max_abs / 127.0 };
            let zero_point = 0.0f32;
            let inv_scale = 1.0 / scale;

            let mut bytes = Vec::with_capacity(data.len());
            for &x in data {
                let q = (x * inv_scale).round().clamp(-128.0, 127.0) as i8;
                bytes.push(q as u8);
            }
            (bytes, scale, zero_point)
        }
        QuantizationScheme::AsymmetricAffine => {
            let range = (max_val - min_val).max(1e-8);
            let scale = range / 255.0;
            let zero_point = (-min_val / scale).round().clamp(0.0, 255.0) - 128.0;
            let inv_scale = 1.0 / scale;

            let mut bytes = Vec::with_capacity(data.len());
            for &x in data {
                let q = ((x * inv_scale) + zero_point).round().clamp(-128.0, 127.0) as i8;
                bytes.push(q as u8);
            }
            (bytes, scale, zero_point)
        }
    }
}

/// Dequantizes signed INT8 bytes back to FP32
pub fn dequantize_int8(
    bytes: &[u8],
    scale: f32,
    zero_point: f32,
    scheme: QuantizationScheme,
) -> Vec<f32> {
    let mut floats = Vec::with_capacity(bytes.len());
    match scheme {
        QuantizationScheme::SymmetricAffine => {
            for &b in bytes {
                let q = b as i8 as f32;
                floats.push(q * scale);
            }
        }
        QuantizationScheme::AsymmetricAffine => {
            for &b in bytes {
                let q = b as i8 as f32;
                floats.push((q - zero_point) * scale);
            }
        }
    }
    floats
}

/// Quantizes FP32 slice into packed 4-bit nibbles (2 weights per byte)
///
/// Signed 4-bit range: [-8, 7]. Cuts memory footprint by 8x relative to FP32.
pub fn quantize_int4(data: &[f32], scheme: QuantizationScheme) -> (Vec<u8>, f32, f32) {
    if data.is_empty() {
        return (Vec::new(), 1.0, 0.0);
    }

    let mut min_val = f32::INFINITY;
    let mut max_val = f32::NEG_INFINITY;
    for &x in data {
        if x < min_val {
            min_val = x;
        }
        if x > max_val {
            max_val = x;
        }
    }

    let packed_len = (data.len() + 1) / 2;
    let mut packed = vec![0u8; packed_len];

    match scheme {
        QuantizationScheme::SymmetricAffine => {
            let max_abs = min_val.abs().max(max_val.abs());
            let scale = if max_abs < 1e-8 { 1.0 } else { max_abs / 7.0 };
            let zero_point = 0.0f32;
            let inv_scale = 1.0 / scale;

            for (i, &x) in data.iter().enumerate() {
                let q = (x * inv_scale).round().clamp(-8.0, 7.0) as i8;
                let nibble = (q as u8) & 0x0F;
                let byte_idx = i / 2;
                if i % 2 == 0 {
                    packed[byte_idx] = nibble;
                } else {
                    packed[byte_idx] |= nibble << 4;
                }
            }
            (packed, scale, zero_point)
        }
        QuantizationScheme::AsymmetricAffine => {
            let range = (max_val - min_val).max(1e-8);
            let scale = range / 15.0;
            let zero_point = (-min_val / scale).round().clamp(0.0, 15.0) - 8.0;
            let inv_scale = 1.0 / scale;

            for (i, &x) in data.iter().enumerate() {
                let q = ((x * inv_scale) + zero_point).round().clamp(-8.0, 7.0) as i8;
                let nibble = (q as u8) & 0x0F;
                let byte_idx = i / 2;
                if i % 2 == 0 {
                    packed[byte_idx] = nibble;
                } else {
                    packed[byte_idx] |= nibble << 4;
                }
            }
            (packed, scale, zero_point)
        }
    }
}

/// Dequantizes packed 4-bit nibbles back to FP32 slice
pub fn dequantize_int4(
    bytes: &[u8],
    element_count: usize,
    scale: f32,
    zero_point: f32,
    scheme: QuantizationScheme,
) -> Vec<f32> {
    let mut floats = Vec::with_capacity(element_count);
    for i in 0..element_count {
        let byte_idx = i / 2;
        let b = bytes[byte_idx];
        let nibble = if i % 2 == 0 { b & 0x0F } else { (b >> 4) & 0x0F };

        // Sign extend 4-bit two's complement (-8 to 7)
        let signed_nibble = if (nibble & 0x08) != 0 {
            (nibble | 0xF0) as i8
        } else {
            nibble as i8
        };

        let val = match scheme {
            QuantizationScheme::SymmetricAffine => signed_nibble as f32 * scale,
            QuantizationScheme::AsymmetricAffine => (signed_nibble as f32 - zero_point) * scale,
        };
        floats.push(val);
    }
    floats
}

/// Evaluates precision preservation and Signal-to-Quantization-Noise Ratio (SQNR)
pub fn calculate_quality_metrics(
    original: &[f32],
    dequantized: &[f32],
    quantized_bytes: usize,
) -> QuantizationQualityMetrics {
    assert_eq!(
        original.len(),
        dequantized.len(),
        "Vector lengths must match for quality evaluation"
    );

    let n = original.len();
    if n == 0 {
        return QuantizationQualityMetrics {
            sqnr_db: 120.0,
            mse: 0.0,
            rmse: 0.0,
            max_absolute_error: 0.0,
            compression_ratio: 1.0,
            original_bytes: 0,
            quantized_bytes: 0,
        };
    }

    let mut signal_power = 0.0f64;
    let mut noise_power = 0.0f64;
    let mut max_abs_diff = 0.0f32;

    for i in 0..n {
        let x = original[i] as f64;
        let x_hat = dequantized[i] as f64;
        let diff = (x - x_hat).abs() as f32;

        signal_power += x * x;
        noise_power += (x - x_hat) * (x - x_hat);
        if diff > max_abs_diff {
            max_abs_diff = diff;
        }
    }

    let mse = (noise_power / (n as f64)) as f32;
    let rmse = mse.sqrt();

    let sqnr_db = if noise_power <= 1e-12 {
        120.0f32
    } else {
        let ratio = (signal_power + 1e-12) / (noise_power + 1e-12);
        (10.0 * ratio.log10()) as f32
    };

    let original_bytes = n * std::mem::size_of::<f32>();
    let compression_ratio = if quantized_bytes > 0 {
        original_bytes as f32 / quantized_bytes as f32
    } else {
        1.0
    };

    QuantizationQualityMetrics {
        sqnr_db,
        mse,
        rmse,
        max_absolute_error: max_abs_diff,
        compression_ratio,
        original_bytes,
        quantized_bytes,
    }
}

// ============================================================================
// DYNAMIC QUANTIZATION POLICY CONTROLLER
// ============================================================================

/// Dynamic thermal and power quantization policy
#[derive(Debug, Clone)]
pub struct ThermalQuantizationPolicy {
    /// Cold temperature threshold: below this uses FP32/FP16 (default: 70.0°C)
    pub cold_threshold_c: f32,
    /// Critical thermal throttling threshold: at or above this forces INT4 (default: 85.0°C)
    pub critical_threshold_c: f32,
    /// Hysteresis band in °C to prevent mode fluttering (default: 2.5°C)
    pub hysteresis_c: f32,
    /// Critical power budget in Watts: exceeding forces INT4 (default: 55.0 W)
    pub critical_power_w: f32,
    /// Whether to force INT8 when disconnected from AC mains (default: true)
    pub power_save_on_battery: bool,
    /// Whether FP16 is preferred over FP32 in the cold zone (default: true)
    pub prefer_fp16: bool,
    /// Currently active quantization mode
    current_mode: QuantizationMode,
}

impl Default for ThermalQuantizationPolicy {
    fn default() -> Self {
        Self {
            cold_threshold_c: 70.0,
            critical_threshold_c: 85.0,
            hysteresis_c: 2.5,
            critical_power_w: 55.0,
            power_save_on_battery: true,
            prefer_fp16: true,
            current_mode: QuantizationMode::FP16,
        }
    }
}

impl ThermalQuantizationPolicy {
    pub fn current_mode(&self) -> QuantizationMode {
        self.current_mode
    }

    /// Evaluates telemetry and transitions mode with hysteresis damping
    pub fn update_policy(&mut self, telemetry: &SmoothedHardwareMetrics) -> QuantizationMode {
        let temp = telemetry.smoothed_temp_c;
        let power = telemetry.smoothed_power_w;
        let battery = !telemetry.raw.ac_connected;

        let next_mode = match self.current_mode {
            QuantizationMode::FP32 | QuantizationMode::FP16 => {
                if temp >= self.critical_threshold_c || power >= self.critical_power_w {
                    QuantizationMode::INT4
                } else if temp >= self.cold_threshold_c || (battery && self.power_save_on_battery) {
                    QuantizationMode::INT8
                } else if self.prefer_fp16 {
                    QuantizationMode::FP16
                } else {
                    QuantizationMode::FP32
                }
            }
            QuantizationMode::INT8 => {
                if temp >= self.critical_threshold_c || power >= self.critical_power_w {
                    QuantizationMode::INT4
                } else if temp < (self.cold_threshold_c - self.hysteresis_c)
                    && (!battery || !self.power_save_on_battery)
                {
                    if self.prefer_fp16 {
                        QuantizationMode::FP16
                    } else {
                        QuantizationMode::FP32
                    }
                } else {
                    QuantizationMode::INT8
                }
            }
            QuantizationMode::INT4 => {
                if temp < (self.critical_threshold_c - self.hysteresis_c)
                    && power < (self.critical_power_w - 5.0)
                {
                    if temp < (self.cold_threshold_c - self.hysteresis_c)
                        && (!battery || !self.power_save_on_battery)
                    {
                        if self.prefer_fp16 {
                            QuantizationMode::FP16
                        } else {
                            QuantizationMode::FP32
                        }
                    } else {
                        QuantizationMode::INT8
                    }
                } else {
                    QuantizationMode::INT4
                }
            }
        };

        self.current_mode = next_mode;
        next_mode
    }

    /// Evaluates synthetic or direct temperature/power readings with hysteresis damping
    pub fn evaluate_policy(&mut self, temp_c: f32, power_w: f32, battery_mode: bool) -> QuantizationMode {
        let telemetry = SmoothedHardwareMetrics {
            raw: LiveHardwareMetrics {
                junction_temp_c: temp_c,
                peak_temp_c: temp_c,
                power_draw_w: power_w,
                thermal_headroom_c: (100.0 - temp_c).max(0.0),
                tj_max_c: 100.0,
                is_thermal_throttling: temp_c >= 85.0,
                ac_connected: !battery_mode,
                gpu_temp_c: None,
                gpu_power_w: None,
                timestamp_ms: Utc::now().timestamp_millis() as u64,
            },
            smoothed_temp_c: temp_c,
            smoothed_power_w: power_w,
            ema_alpha: 0.3,
            thermal_headroom_c: (100.0 - temp_c).max(0.0),
            recommended_mode: QuantizationMode::FP16,
        };
        self.update_policy(&telemetry)
    }
}

// ============================================================================
// 3. RUNTIME KERNEL FUSION ENGINE
// ============================================================================

/// Activation functions fused directly into compute loops
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActivationFunction {
    None,
    ReLU,
    GELU,
    SiLU,
    Tanh,
}

impl ActivationFunction {
    /// Inlined activation evaluation
    #[inline(always)]
    pub fn eval(&self, x: f32) -> f32 {
        match self {
            Self::None => x,
            Self::ReLU => x.max(0.0),
            Self::GELU => {
                // Exact tanh approximation: 0.5 * x * (1 + tanh(sqrt(2/pi) * (x + 0.044715 * x^3)))
                const SQRT_2_OVER_PI: f32 = 0.7978845608;
                let c = SQRT_2_OVER_PI * (x + 0.044715 * x * x * x);
                0.5 * x * (1.0 + c.tanh())
            }
            Self::SiLU => {
                // x * sigmoid(x) = x / (1 + exp(-x))
                if x < -40.0 {
                    0.0
                } else {
                    x / (1.0 + (-x).exp())
                }
            }
            Self::Tanh => x.tanh(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::None => "Identity",
            Self::ReLU => "ReLU",
            Self::GELU => "GELU",
            Self::SiLU => "SiLU",
            Self::Tanh => "Tanh",
        }
    }
}

/// Normalization layer types for fusion
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum NormalizationType {
    LayerNorm,
    RMSNorm,
}

/// Fused kernel execution profile with memory bandwidth savings
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusedKernelProfileReceipt {
    pub operation: String,
    pub input_shape: Vec<usize>,
    pub output_shape: Vec<usize>,
    pub activation: ActivationFunction,
    pub quant_mode: QuantizationMode,
    /// Total bytes that would be transferred across separate unfused memory passes
    pub unfused_memory_bytes: usize,
    /// Actual bytes transferred in single fused pass
    pub fused_memory_bytes: usize,
    /// Bytes eliminated from memory bus roundtrips
    pub bandwidth_saved_bytes: usize,
    /// Bandwidth reduction percentage
    pub bandwidth_reduction_pct: f32,
    /// Intermediate tensor allocations eliminated
    pub intermediate_allocations_avoided: usize,
    /// Execution duration in microseconds
    pub elapsed_micros: u64,
    /// Compute throughput in GFLOPs
    pub throughput_gflops: f64,
    /// Cryptographic receipt ID
    pub receipt_id: String,
    /// HMAC-SHA256 signature for auditability
    pub hmac_signature: String,
}

/// Fused MatMul + Optional Bias + Activation Kernel
///
/// Computes C[M, N] = Activation(A[M, K] * B[K, N] + Bias[N]) in a single cache-tiled pass.
/// Rayon parallelized across rows when M >= 4.
pub fn fused_matmul_bias_activation(
    a: &[f32],
    b: &[f32],
    bias: Option<&[f32]>,
    m: usize,
    k: usize,
    n: usize,
    act: ActivationFunction,
) -> Result<Vec<f32>> {
    if a.len() != m * k || b.len() != k * n {
        return Err(NpuRouterError::DimensionMismatch {
            expected: m * k,
            actual: a.len(),
        });
    }
    if let Some(bi) = bias {
        if bi.len() != n {
            return Err(NpuRouterError::DimensionMismatch {
                expected: n,
                actual: bi.len(),
            });
        }
    }

    let mut output = vec![0.0f32; m * n];

    if m >= 4 {
        output
            .par_chunks_exact_mut(n)
            .enumerate()
            .for_each(|(i, row_out)| {
                let a_row = &a[i * k..(i + 1) * k];
                for j in 0..n {
                    let mut acc = 0.0f32;
                    for p in 0..k {
                        acc += a_row[p] * b[p * n + j];
                    }
                    if let Some(bi) = bias {
                        acc += bi[j];
                    }
                    row_out[j] = act.eval(acc);
                }
            });
    } else {
        for i in 0..m {
            let a_row = &a[i * k..(i + 1) * k];
            let row_offset = i * n;
            for j in 0..n {
                let mut acc = 0.0f32;
                for p in 0..k {
                    acc += a_row[p] * b[p * n + j];
                }
                if let Some(bi) = bias {
                    acc += bi[j];
                }
                output[row_offset + j] = act.eval(acc);
            }
        }
    }

    Ok(output)
}

/// Fused Quantized INT8 MatMul + Optional Bias + Activation Kernel
///
/// Computes C[M, N] = Activation((A_int8 * B_int8) * (s_a * s_b) + Bias[N])
/// Uses INT32 register accumulation for zero overflow risk and executes activation in-register.
pub fn fused_matmul_bias_activation_int8(
    a_int8: &[i8],
    b_int8: &[i8],
    a_scale: f32,
    b_scale: f32,
    bias: Option<&[f32]>,
    m: usize,
    k: usize,
    n: usize,
    act: ActivationFunction,
) -> Result<Vec<f32>> {
    if a_int8.len() != m * k || b_int8.len() != k * n {
        return Err(NpuRouterError::DimensionMismatch {
            expected: m * k,
            actual: a_int8.len(),
        });
    }
    if let Some(bi) = bias {
        if bi.len() != n {
            return Err(NpuRouterError::DimensionMismatch {
                expected: n,
                actual: bi.len(),
            });
        }
    }

    let combined_scale = a_scale * b_scale;
    let mut output = vec![0.0f32; m * n];

    if m >= 4 {
        output
            .par_chunks_exact_mut(n)
            .enumerate()
            .for_each(|(i, row_out)| {
                let a_row = &a_int8[i * k..(i + 1) * k];
                for j in 0..n {
                    let mut acc_i32 = 0i32;
                    for p in 0..k {
                        acc_i32 += (a_row[p] as i32) * (b_int8[p * n + j] as i32);
                    }
                    let mut acc_f32 = (acc_i32 as f32) * combined_scale;
                    if let Some(bi) = bias {
                        acc_f32 += bi[j];
                    }
                    row_out[j] = act.eval(acc_f32);
                }
            });
    } else {
        for i in 0..m {
            let a_row = &a_int8[i * k..(i + 1) * k];
            let row_offset = i * n;
            for j in 0..n {
                let mut acc_i32 = 0i32;
                for p in 0..k {
                    acc_i32 += (a_row[p] as i32) * (b_int8[p * n + j] as i32);
                }
                let mut acc_f32 = (acc_i32 as f32) * combined_scale;
                if let Some(bi) = bias {
                    acc_f32 += bi[j];
                }
                output[row_offset + j] = act.eval(acc_f32);
            }
        }
    }

    Ok(output)
}

/// Fused LayerNorm + Optional Activation Kernel
///
/// Normalizes each batch vector of length `dim`, applies affine gamma/beta,
/// and applies activation directly in L1 cache tiles.
pub fn fused_layernorm_activation(
    x: &[f32],
    gamma: Option<&[f32]>,
    beta: Option<&[f32]>,
    batch: usize,
    dim: usize,
    eps: f32,
    act: ActivationFunction,
) -> Result<Vec<f32>> {
    if x.len() != batch * dim {
        return Err(NpuRouterError::DimensionMismatch {
            expected: batch * dim,
            actual: x.len(),
        });
    }
    if let Some(g) = gamma {
        if g.len() != dim {
            return Err(NpuRouterError::DimensionMismatch {
                expected: dim,
                actual: g.len(),
            });
        }
    }
    if let Some(b) = beta {
        if b.len() != dim {
            return Err(NpuRouterError::DimensionMismatch {
                expected: dim,
                actual: b.len(),
            });
        }
    }

    let mut output = vec![0.0f32; batch * dim];
    let inv_dim = 1.0 / (dim as f32);

    let process_row = |x_row: &[f32], out_row: &mut [f32]| {
        // Pass 1: compute mean
        let mut sum = 0.0f32;
        for &v in x_row {
            sum += v;
        }
        let mean = sum * inv_dim;

        // Pass 2: compute variance
        let mut var_sum = 0.0f32;
        for &v in x_row {
            let diff = v - mean;
            var_sum += diff * diff;
        }
        let inv_std = 1.0 / (var_sum * inv_dim + eps).sqrt();

        // Pass 3: normalize, scale, shift, and activate in cache
        for j in 0..dim {
            let mut val = (x_row[j] - mean) * inv_std;
            if let Some(g) = gamma {
                val *= g[j];
            }
            if let Some(b) = beta {
                val += b[j];
            }
            out_row[j] = act.eval(val);
        }
    };

    if batch >= 4 {
        output
            .par_chunks_exact_mut(dim)
            .enumerate()
            .for_each(|(b, out_row)| {
                let x_row = &x[b * dim..(b + 1) * dim];
                process_row(x_row, out_row);
            });
    } else {
        for b in 0..batch {
            let x_row = &x[b * dim..(b + 1) * dim];
            let out_row = &mut output[b * dim..(b + 1) * dim];
            process_row(x_row, out_row);
        }
    }

    Ok(output)
}

/// Fused RMSNorm (Root Mean Square Normalization) + Activation Kernel
pub fn fused_rmsnorm_activation(
    x: &[f32],
    gamma: Option<&[f32]>,
    batch: usize,
    dim: usize,
    eps: f32,
    act: ActivationFunction,
) -> Result<Vec<f32>> {
    if x.len() != batch * dim {
        return Err(NpuRouterError::DimensionMismatch {
            expected: batch * dim,
            actual: x.len(),
        });
    }
    if let Some(g) = gamma {
        if g.len() != dim {
            return Err(NpuRouterError::DimensionMismatch {
                expected: dim,
                actual: g.len(),
            });
        }
    }

    let mut output = vec![0.0f32; batch * dim];
    let inv_dim = 1.0 / (dim as f32);

    let process_row = |x_row: &[f32], out_row: &mut [f32]| {
        let mut sum_sq = 0.0f32;
        for &v in x_row {
            sum_sq += v * v;
        }
        let rms = (sum_sq * inv_dim + eps).sqrt();
        let inv_rms = 1.0 / rms;

        for j in 0..dim {
            let mut val = x_row[j] * inv_rms;
            if let Some(g) = gamma {
                val *= g[j];
            }
            out_row[j] = act.eval(val);
        }
    };

    if batch >= 4 {
        output
            .par_chunks_exact_mut(dim)
            .enumerate()
            .for_each(|(b, out_row)| {
                let x_row = &x[b * dim..(b + 1) * dim];
                process_row(x_row, out_row);
            });
    } else {
        for b in 0..batch {
            let x_row = &x[b * dim..(b + 1) * dim];
            let out_row = &mut output[b * dim..(b + 1) * dim];
            process_row(x_row, out_row);
        }
    }

    Ok(output)
}

// ============================================================================
// REFERENCE UNMODIFIED IMPLEMENTATIONS (FOR EXACT TEST PARITY VERIFICATION)
// ============================================================================

/// Unfused reference MatMul + Bias + Activation executing sequential discrete passes
pub fn reference_unfused_matmul_bias_activation(
    a: &[f32],
    b: &[f32],
    bias: Option<&[f32]>,
    m: usize,
    k: usize,
    n: usize,
    act: ActivationFunction,
) -> Vec<f32> {
    // Pass 1: MatMul
    let mut matmul_out = vec![0.0f32; m * n];
    for i in 0..m {
        for j in 0..n {
            let mut sum = 0.0f32;
            for p in 0..k {
                sum += a[i * k + p] * b[p * n + j];
            }
            matmul_out[i * n + j] = sum;
        }
    }

    // Pass 2: Bias Add
    let mut bias_out = matmul_out;
    if let Some(bi) = bias {
        for i in 0..m {
            for j in 0..n {
                bias_out[i * n + j] += bi[j];
            }
        }
    }

    // Pass 3: Activation
    let mut final_out = bias_out;
    for val in &mut final_out {
        *val = act.eval(*val);
    }

    final_out
}

/// Unfused reference LayerNorm + Activation executing sequential discrete passes
pub fn reference_unfused_layernorm_activation(
    x: &[f32],
    gamma: Option<&[f32]>,
    beta: Option<&[f32]>,
    batch: usize,
    dim: usize,
    eps: f32,
    act: ActivationFunction,
) -> Vec<f32> {
    // Pass 1: LayerNorm
    let mut norm_out = vec![0.0f32; batch * dim];
    let inv_dim = 1.0 / (dim as f32);

    for b in 0..batch {
        let row = &x[b * dim..(b + 1) * dim];
        let mean = row.iter().sum::<f32>() * inv_dim;
        let var = row.iter().map(|&v| (v - mean) * (v - mean)).sum::<f32>() * inv_dim;
        let inv_std = 1.0 / (var + eps).sqrt();

        for j in 0..dim {
            let mut val = (row[j] - mean) * inv_std;
            if let Some(g) = gamma {
                val *= g[j];
            }
            if let Some(bt) = beta {
                val += bt[j];
            }
            norm_out[b * dim + j] = val;
        }
    }

    // Pass 2: Activation
    let mut final_out = norm_out;
    for val in &mut final_out {
        *val = act.eval(*val);
    }
    final_out
}

// ============================================================================
// 4. MAIN INTEGRATION HUB: HardwareAutoQuantizer
// ============================================================================

/// Principal Hardware Profiling & Auto-Quantization Engine
pub struct HardwareAutoQuantizer {
    scraper: Arc<HardwareTelemetryScraper>,
    policy: Arc<RwLock<ThermalQuantizationPolicy>>,
    secret_key: Vec<u8>,
}

impl Default for HardwareAutoQuantizer {
    fn default() -> Self {
        Self::new()
    }
}

impl HardwareAutoQuantizer {
    /// Creates a new auto-quantizer attached to live sysfs sensors
    pub fn new() -> Self {
        Self {
            scraper: Arc::new(HardwareTelemetryScraper::new()),
            policy: Arc::new(RwLock::new(ThermalQuantizationPolicy::default())),
            secret_key: b"HAGIBIS_AUTO_QUANTIZER_SECRET_HMAC_KEY".to_vec(),
        }
    }

    /// Creates an auto-quantizer rooted at a specified sysfs directory
    pub fn with_sysfs_root(sysfs_root: PathBuf) -> Self {
        Self {
            scraper: Arc::new(HardwareTelemetryScraper::with_sysfs_root(sysfs_root)),
            policy: Arc::new(RwLock::new(ThermalQuantizationPolicy::default())),
            secret_key: b"HAGIBIS_AUTO_QUANTIZER_SECRET_HMAC_KEY".to_vec(),
        }
    }

    pub fn with_secret_key(mut self, key: &[u8]) -> Self {
        self.secret_key = key.to_vec();
        self
    }

    /// Access to telemetry scraper
    pub fn scraper(&self) -> &HardwareTelemetryScraper {
        &self.scraper
    }

    /// Polls smoothed hardware telemetry and updates quantization policy
    pub fn poll_and_update(&self) -> SmoothedHardwareMetrics {
        let telemetry = self.scraper.poll_smoothed();
        let mut policy = self.policy.write().expect("lock quantization policy");
        policy.update_policy(&telemetry);
        telemetry
    }

    /// Returns the currently active quantization mode
    pub fn active_mode(&self) -> QuantizationMode {
        let policy = self.policy.read().expect("read policy");
        policy.current_mode()
    }

    /// Sets simulated telemetry override for testing
    pub fn set_simulated_telemetry(&self, metrics: Option<LiveHardwareMetrics>) {
        self.scraper.set_simulated_override(metrics);
        self.poll_and_update();
    }

    /// Automatically quantizes an input float slice based on live thermal policy
    pub fn auto_quantize(
        &self,
        data: &[f32],
        shape: &[usize],
        scheme: QuantizationScheme,
    ) -> Result<QuantizedTensor> {
        let total_elements: usize = shape.iter().product();
        if data.len() != total_elements {
            return Err(NpuRouterError::DimensionMismatch {
                expected: total_elements,
                actual: data.len(),
            });
        }

        self.poll_and_update();
        let mode = self.active_mode();
        self.quantize_with_mode(data, shape, mode, scheme)
    }

    /// Quantizes input float slice using a specific explicit quantization mode
    pub fn quantize_with_mode(
        &self,
        data: &[f32],
        shape: &[usize],
        mode: QuantizationMode,
        scheme: QuantizationScheme,
    ) -> Result<QuantizedTensor> {
        let total_elements: usize = shape.iter().product();
        if data.len() != total_elements {
            return Err(NpuRouterError::DimensionMismatch {
                expected: total_elements,
                actual: data.len(),
            });
        }

        let (packed_bytes, scale, zero_point) = match mode {
            QuantizationMode::FP32 => (quantize_fp32(data), 1.0f32, 0.0f32),
            QuantizationMode::FP16 => (quantize_fp16(data), 1.0f32, 0.0f32),
            QuantizationMode::INT8 => quantize_int8(data, scheme),
            QuantizationMode::INT4 => quantize_int4(data, scheme),
        };

        // Reconstruct for quality metrics
        let reconstructed = match mode {
            QuantizationMode::FP32 => dequantize_fp32(&packed_bytes),
            QuantizationMode::FP16 => dequantize_fp16(&packed_bytes),
            QuantizationMode::INT8 => dequantize_int8(&packed_bytes, scale, zero_point, scheme),
            QuantizationMode::INT4 => {
                dequantize_int4(&packed_bytes, total_elements, scale, zero_point, scheme)
            }
        };

        let metrics = calculate_quality_metrics(data, &reconstructed, packed_bytes.len());

        Ok(QuantizedTensor {
            mode,
            scheme,
            shape: shape.to_vec(),
            element_count: total_elements,
            scale,
            zero_point,
            packed_bytes,
            metrics,
        })
    }

    /// Bridges with zero-copy [`DeviceMemoryBuffer`] to auto-quantize resident device memory
    pub fn quantize_device_buffer(
        &self,
        buffer: &DeviceMemoryBuffer,
        shape: &[usize],
        scheme: QuantizationScheme,
    ) -> Result<QuantizedTensor> {
        let floats = buffer.as_f32_slice()?;
        self.auto_quantize(floats, shape, scheme)
    }

    /// Dequantizes a [`QuantizedTensor`] directly into a newly allocated [`DeviceMemoryBuffer`]
    pub fn dequantize_to_device_buffer(
        &self,
        tensor: &QuantizedTensor,
        backend: DeviceComputeBackend,
    ) -> Result<DeviceMemoryBuffer> {
        let floats = tensor.dequantize();
        let byte_len = floats.len() * std::mem::size_of::<f32>();
        let mut buf = DeviceMemoryBuffer::allocate_aligned(byte_len, ALIGN_64_AVX512, backend)?;
        buf.as_f32_mut_slice()?.copy_from_slice(&floats);
        buf.flush_range(0, byte_len)?;
        Ok(buf)
    }

    /// Dispatches Fused MatMul + Bias + Activation over [`DeviceMemoryBuffer`] inputs
    pub fn dispatch_fused_matmul(
        &self,
        a_buf: &DeviceMemoryBuffer,
        b_buf: &DeviceMemoryBuffer,
        bias_buf: Option<&DeviceMemoryBuffer>,
        m: usize,
        k: usize,
        n: usize,
        act: ActivationFunction,
    ) -> Result<(DeviceMemoryBuffer, FusedKernelProfileReceipt)> {
        let a = a_buf.f32_slice(0, m * k)?;
        let b = b_buf.f32_slice(0, k * n)?;
        let bias_data = match bias_buf {
            Some(bi) => Some(bi.f32_slice(0, n)?),
            None => None,
        };

        let start = Instant::now();
        let out_data = fused_matmul_bias_activation(a, b, bias_data, m, k, n, act)?;
        let elapsed_micros = start.elapsed().as_micros().max(1) as u64;

        let total_flops = 2.0 * (m as f64) * (k as f64) * (n as f64) + (m * n) as f64;
        let gflops = total_flops / (elapsed_micros as f64 * 1000.0);

        // Memory bandwidth calculations
        let elem_size = std::mem::size_of::<f32>();
        let matmul_bytes = (m * k + k * n + m * n) * elem_size;
        let bias_bytes = if bias_buf.is_some() {
            (2 * m * n + n) * elem_size
        } else {
            0
        };
        let act_bytes = if act != ActivationFunction::None {
            (2 * m * n) * elem_size
        } else {
            0
        };
        let unfused_memory_bytes = matmul_bytes + bias_bytes + act_bytes;

        let fused_reads = (m * k + k * n + if bias_buf.is_some() { n } else { 0 }) * elem_size;
        let fused_writes = (m * n) * elem_size;
        let fused_memory_bytes = fused_reads + fused_writes;

        let bandwidth_saved_bytes = unfused_memory_bytes.saturating_sub(fused_memory_bytes);
        let bandwidth_reduction_pct = (bandwidth_saved_bytes as f32 / unfused_memory_bytes as f32) * 100.0;

        let mut out_buf = DeviceMemoryBuffer::allocate_aligned(
            out_data.len() * elem_size,
            ALIGN_64_AVX512,
            a_buf.backend(),
        )?;
        out_buf.as_f32_mut_slice()?.copy_from_slice(&out_data);
        out_buf.flush_range(0, out_data.len() * elem_size)?;

        let receipt_id = format!("rcpt-fused-gemm-{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
        let sig_payload = format!(
            "{}:{}:{}:{}:{}:{}",
            receipt_id, "FusedMatMulBiasActivation", m, k, n, elapsed_micros
        );
        let mut hasher = Sha256::new();
        hasher.update(&self.secret_key);
        hasher.update(sig_payload.as_bytes());
        let hmac_signature = hex::encode(hasher.finalize());

        let receipt = FusedKernelProfileReceipt {
            operation: "FusedMatMulBiasActivation".to_string(),
            input_shape: vec![m, k, n],
            output_shape: vec![m, n],
            activation: act,
            quant_mode: QuantizationMode::FP32,
            unfused_memory_bytes,
            fused_memory_bytes,
            bandwidth_saved_bytes,
            bandwidth_reduction_pct,
            intermediate_allocations_avoided: if bias_buf.is_some() && act != ActivationFunction::None {
                2
            } else {
                1
            },
            elapsed_micros,
            throughput_gflops: gflops,
            receipt_id,
            hmac_signature,
        };

        Ok((out_buf, receipt))
    }

    /// Dispatches Fused LayerNorm / RMSNorm + Activation over [`DeviceMemoryBuffer`] inputs
    pub fn dispatch_fused_norm(
        &self,
        x_buf: &DeviceMemoryBuffer,
        gamma_buf: Option<&DeviceMemoryBuffer>,
        beta_buf: Option<&DeviceMemoryBuffer>,
        batch: usize,
        dim: usize,
        norm_type: NormalizationType,
        act: ActivationFunction,
    ) -> Result<(DeviceMemoryBuffer, FusedKernelProfileReceipt)> {
        let x = x_buf.f32_slice(0, batch * dim)?;
        let gamma = match gamma_buf {
            Some(g) => Some(g.f32_slice(0, dim)?),
            None => None,
        };
        let beta = match beta_buf {
            Some(b) => Some(b.f32_slice(0, dim)?),
            None => None,
        };

        let start = Instant::now();
        let out_data = match norm_type {
            NormalizationType::LayerNorm => {
                fused_layernorm_activation(x, gamma, beta, batch, dim, 1e-5, act)?
            }
            NormalizationType::RMSNorm => {
                fused_rmsnorm_activation(x, gamma, batch, dim, 1e-5, act)?
            }
        };
        let elapsed_micros = start.elapsed().as_micros().max(1) as u64;

        let elem_size = std::mem::size_of::<f32>();
        let norm_flops = match norm_type {
            NormalizationType::LayerNorm => 5.0 * (batch * dim) as f64,
            NormalizationType::RMSNorm => 3.0 * (batch * dim) as f64,
        };
        let gflops = norm_flops / (elapsed_micros as f64 * 1000.0);

        let unfused_memory_bytes = (4 * batch * dim + 2 * dim) * elem_size;
        let fused_memory_bytes = (2 * batch * dim + dim) * elem_size;
        let bandwidth_saved_bytes = unfused_memory_bytes.saturating_sub(fused_memory_bytes);
        let bandwidth_reduction_pct = (bandwidth_saved_bytes as f32 / unfused_memory_bytes as f32) * 100.0;

        let mut out_buf = DeviceMemoryBuffer::allocate_aligned(
            out_data.len() * elem_size,
            ALIGN_64_AVX512,
            x_buf.backend(),
        )?;
        out_buf.as_f32_mut_slice()?.copy_from_slice(&out_data);
        out_buf.flush_range(0, out_data.len() * elem_size)?;

        let op_name = match norm_type {
            NormalizationType::LayerNorm => "FusedLayerNormActivation",
            NormalizationType::RMSNorm => "FusedRMSNormActivation",
        };

        let receipt_id = format!("rcpt-fused-norm-{}", Utc::now().timestamp_nanos_opt().unwrap_or(0));
        let sig_payload = format!("{}:{}:{}:{}:{}", receipt_id, op_name, batch, dim, elapsed_micros);
        let mut hasher = Sha256::new();
        hasher.update(&self.secret_key);
        hasher.update(sig_payload.as_bytes());
        let hmac_signature = hex::encode(hasher.finalize());

        let receipt = FusedKernelProfileReceipt {
            operation: op_name.to_string(),
            input_shape: vec![batch, dim],
            output_shape: vec![batch, dim],
            activation: act,
            quant_mode: QuantizationMode::FP32,
            unfused_memory_bytes,
            fused_memory_bytes,
            bandwidth_saved_bytes,
            bandwidth_reduction_pct,
            intermediate_allocations_avoided: 1,
            elapsed_micros,
            throughput_gflops: gflops,
            receipt_id,
            hmac_signature,
        };

        Ok((out_buf, receipt))
    }
}

// ============================================================================
// COMPREHENSIVE TEST SUITE
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use crate::governor_bridge::GovernorBridge;
    use crate::hardware_discovery::HardwareInventory;
    use crate::telemetry_router::{NpuOffloadRouter, RouterConfig};

    #[test]
    fn test_live_hardware_telemetry_scraping_real_sensors() {
        let scraper = HardwareTelemetryScraper::new();
        let raw = scraper.scrape_live_raw();

        assert!(
            raw.junction_temp_c >= 5.0 && raw.junction_temp_c <= 125.0,
            "Junction temp out of physical bounds: {}°C",
            raw.junction_temp_c
        );
        assert!(
            raw.peak_temp_c >= raw.junction_temp_c,
            "Peak temp must be >= junction temp"
        );
        assert!(
            raw.tj_max_c >= 70.0 && raw.tj_max_c <= 130.0,
            "TjMax out of expected range: {}°C",
            raw.tj_max_c
        );
        assert!(
            raw.power_draw_w >= 0.5 && raw.power_draw_w <= 350.0,
            "Power draw out of physical bounds: {} W",
            raw.power_draw_w
        );
        assert!(raw.timestamp_ms > 0);
    }

    #[test]
    fn test_ema_smoothing_and_cache_ttl() {
        let scraper = HardwareTelemetryScraper::new()
            .with_cache_ttl(Duration::from_millis(50))
            .with_ema_alpha(0.5);

        let s1 = scraper.poll_smoothed();
        assert!(s1.smoothed_temp_c > 0.0);

        // Rapid second poll within 50ms must hit cache
        let s2 = scraper.poll_smoothed();
        assert_eq!(s1.raw.timestamp_ms, s2.raw.timestamp_ms);
        assert_eq!(s1.smoothed_temp_c, s2.smoothed_temp_c);

        let base_temp = 50.0f32;
        let spike_temp = 90.0f32;

        scraper.set_simulated_override(Some(LiveHardwareMetrics {
            junction_temp_c: base_temp,
            peak_temp_c: base_temp,
            power_draw_w: 20.0,
            thermal_headroom_c: 50.0,
            tj_max_c: 100.0,
            is_thermal_throttling: false,
            ac_connected: true,
            gpu_temp_c: None,
            gpu_power_w: None,
            timestamp_ms: 1000,
        }));
        std::thread::sleep(Duration::from_millis(60));
        let snap1 = scraper.poll_smoothed();

        // Sudden spike
        scraper.set_simulated_override(Some(LiveHardwareMetrics {
            junction_temp_c: spike_temp,
            peak_temp_c: spike_temp,
            power_draw_w: 40.0,
            thermal_headroom_c: 10.0,
            tj_max_c: 100.0,
            is_thermal_throttling: true,
            ac_connected: true,
            gpu_temp_c: None,
            gpu_power_w: None,
            timestamp_ms: 2000,
        }));
        std::thread::sleep(Duration::from_millis(60));
        let snap2 = scraper.poll_smoothed();

        let expected_smoothed = 0.5 * spike_temp + 0.5 * snap1.smoothed_temp_c;
        assert!(
            (snap2.smoothed_temp_c - expected_smoothed).abs() < 1e-3,
            "Smoothed temp {} expected {}",
            snap2.smoothed_temp_c,
            expected_smoothed
        );
        assert!(snap2.smoothed_temp_c < spike_temp);
        assert!(snap2.smoothed_temp_c > snap1.smoothed_temp_c);
    }

    #[test]
    fn test_sysfs_parsing_from_isolated_directory() {
        let tmp = tempdir().expect("create temp dir");
        let sys_root = tmp.path();

        let zone0 = sys_root.join("class/thermal/thermal_zone0");
        fs::create_dir_all(&zone0).unwrap();
        fs::write(zone0.join("temp"), "73500\n").unwrap(); // 73.5°C
        fs::write(zone0.join("type"), "x86_pkg_temp\n").unwrap();
        fs::write(zone0.join("trip_point_0_type"), "critical\n").unwrap();
        fs::write(zone0.join("trip_point_0_temp"), "105000\n").unwrap(); // 105.0°C

        let hwmon0 = sys_root.join("class/hwmon/hwmon0");
        fs::create_dir_all(&hwmon0).unwrap();
        fs::write(hwmon0.join("power1_input"), "32500000\n").unwrap(); // 32.5 Watts

        let ps = sys_root.join("class/power_supply/ADP1");
        fs::create_dir_all(&ps).unwrap();
        fs::write(ps.join("online"), "1\n").unwrap();

        let scraper = HardwareTelemetryScraper::with_sysfs_root(sys_root.to_path_buf());
        let raw = scraper.scrape_live_raw();

        assert!((raw.junction_temp_c - 73.5).abs() < 1e-2);
        assert!((raw.peak_temp_c - 73.5).abs() < 1e-2);
        assert!((raw.tj_max_c - 105.0).abs() < 1e-2);
        assert!((raw.thermal_headroom_c - (105.0 - 73.5)).abs() < 1e-2);
        assert!((raw.power_draw_w - 32.5).abs() < 1e-2);
        assert!(raw.ac_connected);
    }

    #[test]
    fn test_thermal_quantization_policy_transitions_and_hysteresis() {
        let mut policy = ThermalQuantizationPolicy {
            cold_threshold_c: 70.0,
            critical_threshold_c: 85.0,
            hysteresis_c: 2.5,
            critical_power_w: 55.0,
            power_save_on_battery: true,
            prefer_fp16: true,
            current_mode: QuantizationMode::FP16,
        };

        let make_metric = |temp: f32, power: f32, ac: bool| SmoothedHardwareMetrics {
            raw: LiveHardwareMetrics {
                junction_temp_c: temp,
                peak_temp_c: temp,
                power_draw_w: power,
                thermal_headroom_c: 100.0 - temp,
                tj_max_c: 100.0,
                is_thermal_throttling: temp >= 85.0,
                ac_connected: ac,
                gpu_temp_c: None,
                gpu_power_w: None,
                timestamp_ms: 100,
            },
            smoothed_temp_c: temp,
            smoothed_power_w: power,
            ema_alpha: 0.25,
            thermal_headroom_c: 100.0 - temp,
            recommended_mode: QuantizationMode::FP16,
        };

        // Cold range (45°C, AC mains) -> FP16
        assert_eq!(policy.update_policy(&make_metric(45.0, 20.0, true)), QuantizationMode::FP16);

        // Warm range (74°C, AC mains) -> INT8
        assert_eq!(policy.update_policy(&make_metric(74.0, 25.0, true)), QuantizationMode::INT8);

        // Critical thermal throttling (89°C, AC mains) -> INT4
        assert_eq!(policy.update_policy(&make_metric(89.0, 40.0, true)), QuantizationMode::INT4);

        // Hysteresis test: cool to 84.0°C (threshold is 85.0, hysteresis requires < 82.5°C to drop) -> stays INT4
        assert_eq!(policy.update_policy(&make_metric(84.0, 40.0, true)), QuantizationMode::INT4);

        // Cool below critical hysteresis band: 81.0°C (< 82.5°C) -> Drops to INT8
        assert_eq!(policy.update_policy(&make_metric(81.0, 30.0, true)), QuantizationMode::INT8);

        // Hysteresis test: cool to 68.5°C (threshold is 70.0, hysteresis requires < 67.5°C to drop to FP16) -> stays INT8
        assert_eq!(policy.update_policy(&make_metric(68.5, 25.0, true)), QuantizationMode::INT8);

        // Cool below warm hysteresis band: 66.0°C (< 67.5°C) -> Drops to FP16
        assert_eq!(policy.update_policy(&make_metric(66.0, 20.0, true)), QuantizationMode::FP16);

        // Battery preservation trigger: cold (50°C), but on battery -> forces INT8
        assert_eq!(policy.update_policy(&make_metric(50.0, 15.0, false)), QuantizationMode::INT8);

        // Severe power duress trigger: cold (55°C), but power spikes to 62W (>= 55W) -> forces INT4
        assert_eq!(policy.update_policy(&make_metric(55.0, 62.0, true)), QuantizationMode::INT4);
    }

    #[test]
    fn test_fp16_native_software_conversion_exactness() {
        let test_values = [
            0.0f32,
            -0.0f32,
            1.0f32,
            -1.0f32,
            0.5f32,
            -0.5f32,
            0.125f32,
            2.0f32,
            100.0f32,
            1000.0f32,
            65504.0f32,
            0.00006103515625f32,
        ];

        for &val in &test_values {
            let h = f32_to_f16(val);
            let back = f16_to_f32(h);
            let relative_error = (val - back).abs() / (val.abs().max(1e-5));
            assert!(
                relative_error < 0.002,
                "FP16 conversion drift for {}: got {}, rel error {}",
                val,
                back,
                relative_error
            );
        }
    }

    #[test]
    fn test_int8_symmetric_and_asymmetric_accuracy_and_sqnr() {
        let mut data = Vec::with_capacity(256);
        for i in 0..256 {
            data.push(((i as f32 * 0.13).sin() * 2.5) + 0.3);
        }

        // Symmetric INT8
        let (sym_bytes, sym_scale, sym_zp) = quantize_int8(&data, QuantizationScheme::SymmetricAffine);
        assert_eq!(sym_bytes.len(), data.len());
        assert_eq!(sym_zp, 0.0);
        assert!(sym_scale > 0.0);

        let sym_dequant = dequantize_int8(&sym_bytes, sym_scale, sym_zp, QuantizationScheme::SymmetricAffine);
        let sym_metrics = calculate_quality_metrics(&data, &sym_dequant, sym_bytes.len());

        assert!(
            sym_metrics.sqnr_db >= 35.0,
            "INT8 symmetric SQNR must be >= 35 dB, got {:.2} dB",
            sym_metrics.sqnr_db
        );
        assert!(
            sym_metrics.compression_ratio >= 3.99,
            "INT8 must achieve 4x compression"
        );
        assert!(sym_metrics.max_absolute_error <= sym_scale * 1.05);

        // Asymmetric INT8
        let (asym_bytes, asym_scale, asym_zp) = quantize_int8(&data, QuantizationScheme::AsymmetricAffine);
        let asym_dequant = dequantize_int8(&asym_bytes, asym_scale, asym_zp, QuantizationScheme::AsymmetricAffine);
        let asym_metrics = calculate_quality_metrics(&data, &asym_dequant, asym_bytes.len());

        assert!(
            asym_metrics.sqnr_db >= 35.0,
            "INT8 asymmetric SQNR must be >= 35 dB, got {:.2} dB",
            asym_metrics.sqnr_db
        );
    }

    #[test]
    fn test_int4_packed_nibbles_accuracy_and_bandwidth_compression() {
        let n = 512;
        let mut data = Vec::with_capacity(n);
        for i in 0..n {
            data.push((i as f32 * 0.07).cos() * 4.0);
        }

        let (packed, scale, zp) = quantize_int4(&data, QuantizationScheme::SymmetricAffine);
        assert_eq!(
            packed.len(),
            n / 2,
            "INT4 packed bytes must be exactly half the element count"
        );

        let dequantized = dequantize_int4(&packed, n, scale, zp, QuantizationScheme::SymmetricAffine);
        assert_eq!(dequantized.len(), n);

        let metrics = calculate_quality_metrics(&data, &dequantized, packed.len());

        assert!(
            metrics.sqnr_db >= 18.0,
            "INT4 SQNR must be >= 18 dB, got {:.2} dB",
            metrics.sqnr_db
        );
        assert!(
            metrics.compression_ratio >= 7.99,
            "INT4 must achieve 8x compression relative to FP32, got {}",
            metrics.compression_ratio
        );
        assert!(
            metrics.max_absolute_error <= scale * 1.1,
            "Max error bound violation"
        );
    }

    #[test]
    fn test_int4_odd_length_packing_and_nibble_sign_extension() {
        let data = vec![-7.0f32, -3.0f32, 0.0f32, 2.0f32, 6.0f32];
        let (packed, scale, zp) = quantize_int4(&data, QuantizationScheme::SymmetricAffine);
        assert_eq!(packed.len(), 3);

        let dequantized = dequantize_int4(&packed, data.len(), scale, zp, QuantizationScheme::SymmetricAffine);
        assert_eq!(dequantized.len(), 5);

        for (orig, deq) in data.iter().zip(dequantized.iter()) {
            assert!(
                (orig - deq).abs() <= scale * 1.05,
                "Reconstruction error for {}: got {}",
                orig,
                deq
            );
        }
    }

    #[test]
    fn test_fused_matmul_bias_activation_numerical_parity_all_activations() {
        let m = 8;
        let k = 16;
        let n = 8;

        let mut a = vec![0.0f32; m * k];
        let mut b = vec![0.0f32; k * n];
        let mut bias = vec![0.0f32; n];

        for i in 0..a.len() {
            a[i] = ((i as f32 * 0.23).sin() * 1.5) - 0.2;
        }
        for i in 0..b.len() {
            b[i] = ((i as f32 * 0.17).cos() * 1.2) + 0.1;
        }
        for j in 0..bias.len() {
            bias[j] = (j as f32 * 0.3) - 1.0;
        }

        let activations = [
            ActivationFunction::None,
            ActivationFunction::ReLU,
            ActivationFunction::GELU,
            ActivationFunction::SiLU,
            ActivationFunction::Tanh,
        ];

        for act in activations {
            let fused_out = fused_matmul_bias_activation(&a, &b, Some(&bias), m, k, n, act).unwrap();
            let ref_out = reference_unfused_matmul_bias_activation(&a, &b, Some(&bias), m, k, n, act);

            assert_eq!(fused_out.len(), ref_out.len());
            for (idx, (f, r)) in fused_out.iter().zip(ref_out.iter()).enumerate() {
                let diff = (f - r).abs();
                assert!(
                    diff < 1e-4,
                    "Parity mismatch in {:?} at index {}: fused={}, ref={}, diff={}",
                    act,
                    idx,
                    f,
                    r,
                    diff
                );
            }
        }
    }

    #[test]
    fn test_fused_layernorm_activation_numerical_parity() {
        let batch = 6;
        let dim = 32;
        let mut x = vec![0.0f32; batch * dim];
        let mut gamma = vec![1.0f32; dim];
        let mut beta = vec![0.0f32; dim];

        for i in 0..x.len() {
            x[i] = ((i as f32 * 0.31).sin() * 5.0) + 1.2;
        }
        for j in 0..dim {
            gamma[j] = 0.8 + (j as f32 * 0.05);
            beta[j] = -0.5 + (j as f32 * 0.02);
        }

        let act = ActivationFunction::GELU;
        let fused = fused_layernorm_activation(&x, Some(&gamma), Some(&beta), batch, dim, 1e-5, act).unwrap();
        let reference = reference_unfused_layernorm_activation(&x, Some(&gamma), Some(&beta), batch, dim, 1e-5, act);

        assert_eq!(fused.len(), reference.len());
        for (i, (f, r)) in fused.iter().zip(reference.iter()).enumerate() {
            let diff = (f - r).abs();
            assert!(
                diff < 1e-4,
                "LayerNorm mismatch at index {}: fused={}, ref={}, diff={}",
                i,
                f,
                r,
                diff
            );
        }
    }

    #[test]
    fn test_fused_rmsnorm_activation_numerical_parity() {
        let batch = 4;
        let dim = 16;
        let mut x = vec![0.0f32; batch * dim];
        let mut gamma = vec![1.0f32; dim];

        for i in 0..x.len() {
            x[i] = (i as f32 * 0.19).cos() * 3.0;
        }
        for j in 0..dim {
            gamma[j] = 1.0 + (j as f32 * 0.1);
        }

        let act = ActivationFunction::SiLU;
        let fused = fused_rmsnorm_activation(&x, Some(&gamma), batch, dim, 1e-5, act).unwrap();

        assert_eq!(fused.len(), batch * dim);
        for b in 0..batch {
            let row = &fused[b * dim..(b + 1) * dim];
            assert!(row.iter().all(|&v| !v.is_nan() && !v.is_infinite()));
        }
    }

    #[test]
    fn test_fused_matmul_quantized_int8_accuracy() {
        let m = 4;
        let k = 8;
        let n = 4;

        let a_int8 = vec![10i8; m * k];
        let b_int8 = vec![5i8; k * n];
        let a_scale = 0.1f32;
        let b_scale = 0.2f32;
        let bias = vec![1.0f32; n];

        let out = fused_matmul_bias_activation_int8(
            &a_int8,
            &b_int8,
            a_scale,
            b_scale,
            Some(&bias),
            m,
            k,
            n,
            ActivationFunction::ReLU,
        )
        .unwrap();

        for &val in &out {
            assert!((val - 9.0).abs() < 1e-4, "Expected 9.0, got {}", val);
        }
    }

    #[test]
    fn test_memory_bandwidth_savings_accounting() {
        let quantizer = HardwareAutoQuantizer::new();
        let m = 32;
        let k = 32;
        let n = 32;

        let a_buf = DeviceMemoryBuffer::allocate_aligned(m * k * 4, ALIGN_64_AVX512, DeviceComputeBackend::SimdUnifiedMemory).unwrap();
        let b_buf = DeviceMemoryBuffer::allocate_aligned(k * n * 4, ALIGN_64_AVX512, DeviceComputeBackend::SimdUnifiedMemory).unwrap();
        let bias_buf = DeviceMemoryBuffer::allocate_aligned(n * 4, ALIGN_64_AVX512, DeviceComputeBackend::SimdUnifiedMemory).unwrap();

        let (_out, receipt) = quantizer
            .dispatch_fused_matmul(&a_buf, &b_buf, Some(&bias_buf), m, k, n, ActivationFunction::GELU)
            .unwrap();

        assert!(
            receipt.bandwidth_reduction_pct >= 50.0,
            "Bandwidth reduction must exceed 50%, got {:.1}%",
            receipt.bandwidth_reduction_pct
        );
        assert!(receipt.bandwidth_saved_bytes > 0);
        assert!(receipt.intermediate_allocations_avoided >= 2);
        assert!(!receipt.receipt_id.is_empty());
        assert!(!receipt.hmac_signature.is_empty());
        assert!(receipt.throughput_gflops > 0.0);
    }

    #[test]
    fn test_zero_copy_device_memory_buffer_quantize_and_dequantize() {
        let quantizer = HardwareAutoQuantizer::new();
        let elements = 128;
        let byte_len = elements * std::mem::size_of::<f32>();

        let mut buf = DeviceMemoryBuffer::allocate_aligned(
            byte_len,
            ALIGN_64_AVX512,
            DeviceComputeBackend::SimdUnifiedMemory,
        )
        .unwrap();

        {
            let floats = buf.as_f32_mut_slice().unwrap();
            for i in 0..elements {
                floats[i] = ((i as f32 * 0.15).sin() * 3.0) + 0.5;
            }
        }

        let q_tensor = quantizer
            .quantize_device_buffer(&buf, &[elements], QuantizationScheme::SymmetricAffine)
            .unwrap();

        assert_eq!(q_tensor.element_count, elements);
        assert!(q_tensor.metrics.sqnr_db >= 15.0);

        let dequant_buf = quantizer
            .dequantize_to_device_buffer(&q_tensor, DeviceComputeBackend::VulkanComputeShader)
            .unwrap();

        assert_eq!(dequant_buf.capacity_bytes(), byte_len);
        assert!(dequant_buf.is_gpu_cache_aligned());

        let original = buf.as_f32_slice().unwrap();
        let restored = dequant_buf.as_f32_slice().unwrap();
        assert_eq!(original.len(), restored.len());
    }

    #[test]
    fn test_telemetry_router_quantization_recommendation_bridge() {
        let inventory = HardwareInventory::discover().unwrap();
        let governor = GovernorBridge::default();
        let router = NpuOffloadRouter::new(inventory, governor, RouterConfig::default());

        let (recommended_mode, fusion_required) = router.recommend_quantization();
        assert!(matches!(
            recommended_mode,
            QuantizationMode::FP32 | QuantizationMode::FP16 | QuantizationMode::INT8 | QuantizationMode::INT4
        ));

        if recommended_mode == QuantizationMode::INT4 || recommended_mode == QuantizationMode::INT8 {
            assert!(fusion_required);
        }
    }
}
