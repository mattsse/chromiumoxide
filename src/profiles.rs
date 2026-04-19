//! Stealth profile system for customizable browser fingerprints.
//!
//! This module provides an ergonomic builder pattern for creating consistent
//! browser "personalities" that bypass anti-bot detection.
//!
//! # Example
//!
//! ```rust
//! use chaser_oxide::profiles::{ChaserProfile, Gpu};
//!
//! let profile = ChaserProfile::windows()
//!     .chrome_version(130)
//!     .gpu(Gpu::NvidiaRTX4080)
//!     .memory_gb(16)
//!     .cpu_cores(12)
//!     .build();
//! ```

use std::fmt;

/// GPU presets for WebGL spoofing
#[derive(Debug, Clone, Copy)]
pub enum Gpu {
    /// NVIDIA GeForce RTX 3080 (high-trust gaming GPU)
    NvidiaRTX3080,
    /// NVIDIA GeForce RTX 4080 (newer gaming GPU)
    NvidiaRTX4080,
    /// NVIDIA GeForce GTX 1660 (mid-range GPU)
    NvidiaGTX1660,
    /// Intel UHD Graphics 630 (common laptop GPU)
    IntelUHD630,
    /// Intel Iris Xe (modern laptop GPU)
    IntelIrisXe,
    /// Apple M1 Pro
    AppleM1Pro,
    /// Apple M2 Max
    AppleM2Max,
    /// Apple M4 Max
    AppleM4Max,
    /// AMD Radeon RX 6800
    AmdRadeonRX6800,
}

impl Gpu {
    /// Returns the WebGL vendor string
    pub fn vendor(&self) -> &'static str {
        match self {
            Gpu::NvidiaRTX3080 | Gpu::NvidiaRTX4080 | Gpu::NvidiaGTX1660 => "Google Inc. (NVIDIA)",
            Gpu::IntelUHD630 | Gpu::IntelIrisXe => "Google Inc. (Intel)",
            Gpu::AppleM1Pro | Gpu::AppleM2Max | Gpu::AppleM4Max => "Google Inc. (Apple)",
            Gpu::AmdRadeonRX6800 => "Google Inc. (AMD)",
        }
    }

    /// Returns the WebGL renderer string
    pub fn renderer(&self) -> &'static str {
        match self {
            Gpu::NvidiaRTX3080 => {
                "ANGLE (NVIDIA, NVIDIA GeForce RTX 3080 Direct3D11 vs_5_0 ps_5_0)"
            }
            Gpu::NvidiaRTX4080 => {
                "ANGLE (NVIDIA, NVIDIA GeForce RTX 4080 Direct3D11 vs_5_0 ps_5_0)"
            }
            Gpu::NvidiaGTX1660 => {
                "ANGLE (NVIDIA, NVIDIA GeForce GTX 1660 SUPER Direct3D11 vs_5_0 ps_5_0)"
            }
            Gpu::IntelUHD630 => "ANGLE (Intel, Intel(R) UHD Graphics 630 Direct3D11 vs_5_0 ps_5_0)",
            Gpu::IntelIrisXe => {
                "ANGLE (Intel, Intel(R) Iris(R) Xe Graphics Direct3D11 vs_5_0 ps_5_0)"
            }
            Gpu::AppleM1Pro => "ANGLE (Apple, Apple M1 Pro, OpenGL 4.1)",
            Gpu::AppleM2Max => "ANGLE (Apple, Apple M2 Max, OpenGL 4.1)",
            Gpu::AppleM4Max => {
                "ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Max, Unspecified Version)"
            }
            Gpu::AmdRadeonRX6800 => "ANGLE (AMD, AMD Radeon RX 6800 XT Direct3D11 vs_5_0 ps_5_0)",
        }
    }
}

/// Operating system presets
#[derive(Debug, Clone, Copy)]
pub enum Os {
    /// Windows 10/11 64-bit
    Windows,
    /// macOS (Intel)
    MacOSIntel,
    /// macOS (Apple Silicon)
    MacOSArm,
    /// Linux x86_64
    Linux,
}

impl Os {
    /// Returns the navigator.platform value
    pub fn platform(&self) -> &'static str {
        match self {
            Os::Windows => "Win32",
            Os::MacOSIntel | Os::MacOSArm => "MacIntel",
            Os::Linux => "Linux x86_64",
        }
    }

    /// Returns the client hints platform
    pub fn hints_platform(&self) -> &'static str {
        match self {
            Os::Windows => "Windows",
            Os::MacOSIntel | Os::MacOSArm => "macOS",
            Os::Linux => "Linux",
        }
    }

    /// Returns the UA-CH platformVersion string
    pub fn platform_version(&self) -> &'static str {
        match self {
            Os::Windows => "15.0.0",             // Windows 11
            Os::MacOSIntel | Os::MacOSArm => "15.3.1", // macOS Sequoia
            Os::Linux => "",
        }
    }

    /// Returns the UA-CH architecture string
    pub fn architecture(&self) -> &'static str {
        match self {
            Os::Windows | Os::MacOSIntel | Os::Linux => "x86",
            Os::MacOSArm => "arm",
        }
    }
}

/// A builder for creating consistent browser fingerprint profiles.
///
/// # Example
///
/// ```rust
/// use chaser_oxide::profiles::{ChaserProfile, Gpu, Os};
///
/// // Quick preset
/// let profile = ChaserProfile::windows().build();
///
/// // Customized
/// let profile = ChaserProfile::new(Os::Windows)
///     .chrome_version(130)
///     .gpu(Gpu::NvidiaRTX4080)
///     .memory_gb(32)
///     .cpu_cores(16)
///     .locale("de-DE")
///     .timezone("Europe/Berlin")
///     .build();
/// ```
#[derive(Debug, Clone)]
pub struct ChaserProfile {
    os: Os,
    chrome_version: u32,
    gpu: Gpu,
    memory_gb: u32,
    cpu_cores: u32,
    locale: String,
    timezone: String,
    screen_width: u32,
    screen_height: u32,
}

impl Default for ChaserProfile {
    fn default() -> Self {
        Self::native().build()
    }
}

impl ChaserProfile {
    /// Create a new profile builder with the specified OS
    #[allow(clippy::new_ret_no_self)]
    pub fn new(os: Os) -> ChaserProfileBuilder {
        ChaserProfileBuilder {
            os,
            chrome_version: 129,
            gpu: match os {
                Os::Windows => Gpu::NvidiaRTX3080,
                Os::MacOSIntel => Gpu::AppleM1Pro,
                Os::MacOSArm => Gpu::AppleM4Max,
                Os::Linux => Gpu::NvidiaGTX1660,
            },
            memory_gb: 8,
            cpu_cores: 8,
            locale: "en-US".to_string(),
            timezone: "America/New_York".to_string(),
            screen_width: 1920,
            screen_height: 1080,
        }
    }

    /// Create a Windows profile with sensible defaults (RTX 3080, 8 cores)
    pub fn windows() -> ChaserProfileBuilder {
        Self::new(Os::Windows)
    }

    /// Create a macOS Intel profile
    pub fn macos_intel() -> ChaserProfileBuilder {
        Self::new(Os::MacOSIntel).gpu(Gpu::AppleM1Pro)
    }

    /// Create a macOS Apple Silicon profile
    pub fn macos_arm() -> ChaserProfileBuilder {
        Self::new(Os::MacOSArm).gpu(Gpu::AppleM4Max)
    }

    /// Create a Linux profile
    pub fn linux() -> ChaserProfileBuilder {
        Self::new(Os::Linux)
    }

    /// Create a profile matching the current host OS, with optional Chrome version auto-detection.
    /// Reads actual system RAM and tries to detect the installed Chrome version.
    pub fn native() -> ChaserProfileBuilder {
        let os = detect_current_os();
        let chrome = detect_chrome_version().unwrap_or(131);
        let memory = detect_system_memory_gb();
        Self::new(os).chrome_version(chrome).memory_gb(memory)
    }

    // Getters
    pub fn os(&self) -> Os {
        self.os
    }
    pub fn chrome_version(&self) -> u32 {
        self.chrome_version
    }
    pub fn gpu(&self) -> Gpu {
        self.gpu
    }
    pub fn memory_gb(&self) -> u32 {
        self.memory_gb
    }
    pub fn cpu_cores(&self) -> u32 {
        self.cpu_cores
    }
    pub fn locale(&self) -> &str {
        &self.locale
    }
    pub fn timezone(&self) -> &str {
        &self.timezone
    }
    pub fn screen_width(&self) -> u32 {
        self.screen_width
    }
    pub fn screen_height(&self) -> u32 {
        self.screen_height
    }

    /// Returns the valid `navigator.deviceMemory` value (spec allows: 0.25, 0.5, 1, 2, 4, 8).
    fn device_memory_value(&self) -> f32 {
        match self.memory_gb {
            0 => 0.25,
            1 => 1.0,
            2 => 2.0,
            3 | 4 => 4.0,
            _ => 8.0,
        }
    }

    /// Generate the User-Agent string for this profile
    pub fn user_agent(&self) -> String {
        let os_part = match self.os {
            Os::Windows => "Windows NT 10.0; Win64; x64",
            Os::MacOSIntel | Os::MacOSArm => "Macintosh; Intel Mac OS X 10_15_7",
            Os::Linux => "X11; Linux x86_64",
        };
        format!(
            "Mozilla/5.0 ({}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{}.0.0.0 Safari/537.36",
            os_part, self.chrome_version
        )
    }

    /// Generate the complete JavaScript bootstrap script for this profile
    pub fn bootstrap_script(&self) -> String {
        let mut script = format!(
            r#"
            (function() {{
                // === chaser-oxide HARDWARE HARMONY ===
                // Profile: {ua}

                // 0. CDP Marker Cleanup (run once at startup)
                for (const prop of Object.getOwnPropertyNames(window)) {{
                    if (/^cdc_|^\$cdc_|^__webdriver|^__selenium|^__driver|^\$chrome_/.test(prop)) {{
                        try {{ delete window[prop]; }} catch(e) {{}}
                    }}
                }}

                // Prevent CDP detection via Error.prepareStackTrace
                const OriginalError = Error;  
                const originalPrepareStackTrace = Error.prepareStackTrace;    
                let currentPrepareStackTrace = originalPrepareStackTrace;    
                Object.defineProperty(Error, 'prepareStackTrace', {{    
                    get() {{
                        return currentPrepareStackTrace;   
                    }},  
                    set(fn) {{ 
                        // do nothing to prevent detection of CDP
                    }},    
                    configurable: true,    
                    enumerable: false  
                }});

                // 1. Platform (on prototype to avoid getOwnPropertyNames detection)
                Object.defineProperty(Navigator.prototype, 'platform', {{
                    get: () => '{platform}',
                    configurable: true
                }});

                // 2. Hardware (on prototype)
                Object.defineProperty(Navigator.prototype, 'hardwareConcurrency', {{
                    get: () => {cores},
                    configurable: true
                }});
                Object.defineProperty(Navigator.prototype, 'deviceMemory', {{
                    get: () => {device_memory},
                    configurable: true
                }});
                Object.defineProperty(Navigator.prototype, 'maxTouchPoints', {{
                    get: () => 0,
                    configurable: true
                }});

                // 3. WebGL
                const spoofWebGL = (proto) => {{
                    const getParameter = proto.getParameter;
                    proto.getParameter = function(parameter) {{
                        if (parameter === 37445) return '{webgl_vendor}';
                        if (parameter === 37446) return '{webgl_renderer}';
                        return getParameter.apply(this, arguments);
                    }};
                }};
                spoofWebGL(WebGLRenderingContext.prototype);
                if (typeof WebGL2RenderingContext !== 'undefined') {{
                    spoofWebGL(WebGL2RenderingContext.prototype);
                }}

                // 4. Client Hints (on prototype)
                Object.defineProperty(Navigator.prototype, 'userAgentData', {{
                    get: () => ({{
                        brands: [
                            {{ brand: "Google Chrome", version: "{chrome_ver}" }},
                            {{ brand: "Chromium", version: "{chrome_ver}" }},
                            {{ brand: "Not=A?Brand", version: "24" }}
                        ],
                        mobile: false,
                        platform: "{hints_platform}"
                    }}),
                    configurable: true
                }});

                Object.defineProperty(Navigator.prototype.userAgentData.__proto__, 'getHighEntropyValues', {{
                    value: async function(hints) {{
                        const values = {{}};
                        for (const hint of hints) {{
                            if (hint === 'platform') values.platform = "{hints_platform}";
                            else if (hint === 'platformVersion') values.platformVersion = "{platform_version}";
                            else if (hint === 'architecture') values.architecture = "{architecture}";
                            else if (hint === 'model') values.model = "";
                            else if (hint === 'bitness') values.bitness = "64";
                            else if (hint === 'uaFullVersion') values.uaFullVersion = "{chrome_ver}.0.0.0";
                        }}
                        return values;

                    }},
                    configurable: true
                }});

                // 5. Video Codecs
                const canPlayType = HTMLMediaElement.prototype.canPlayType;
                HTMLMediaElement.prototype.canPlayType = function(type) {{
                    if (type.includes('avc1')) return 'probably';
                    if (type.includes('mp4a.40')) return 'probably';
                    if (type === 'video/mp4') return 'probably';
                    return canPlayType.apply(this, arguments);
                }};

                // 6. WebDriver (set to false instead of delete - more realistic)
                Object.defineProperty(Object.getPrototypeOf(navigator), 'webdriver', {{
                    get: () => false,
                    configurable: true,
                    enumerable: true
                }});

                // 7. Chrome Object (enhanced with runtime APIs)
                if (!window.chrome) {{
                    window.chrome = {{}};
                }}
                if (!window.chrome.runtime) {{
                    window.chrome.runtime = {{}};
                }}
                
                // Chrome Runtime APIs (required by Turnstile)
                if (!window.chrome.runtime.connect) {{
                    window.chrome.runtime.connect = function() {{
                        return {{
                            name: '',
                            sender: undefined,
                            onDisconnect: {{ 
                                addListener: function() {{}}, 
                                removeListener: function() {{}},
                                hasListener: function() {{ return false; }},
                                hasListeners: function() {{ return false; }}
                            }},
                            onMessage: {{ 
                                addListener: function() {{}}, 
                                removeListener: function() {{}},
                                hasListener: function() {{ return false; }},
                                hasListeners: function() {{ return false; }}
                            }},
                            postMessage: function() {{}},
                            disconnect: function() {{}}
                        }};
                    }};
                }}
                if (!window.chrome.runtime.sendMessage) {{
                    window.chrome.runtime.sendMessage = function() {{ return; }};
                }}

                // Chrome CSI (Chrome Speed Index) - some sites check this
                if (!window.chrome.csi) {{
                    window.chrome.csi = function() {{
                        const now = Date.now();
                        return {{ 
                            startE: now, 
                            onloadT: now, 
                            pageT: now, 
                            tran: 15 
                        }};
                    }};
                }}

                // Chrome loadTimes (deprecated but still checked)
                if (!window.chrome.loadTimes) {{
                    window.chrome.loadTimes = function() {{
                        const now = Date.now() / 1000;
                        return {{
                            requestTime: now,
                            startLoadTime: now,
                            commitLoadTime: now,
                            finishDocumentLoadTime: now,
                            finishLoadTime: now,
                            firstPaintTime: now,
                            firstPaintAfterLoadTime: 0,
                            navigationType: "Other",
                            wasFetchedViaSpdy: false,
                            wasNpnNegotiated: false,
                            npnNegotiatedProtocol: "",
                            wasAlternateProtocolAvailable: false,
                            connectionInfo: "http/1.1"
                        }};
                    }};
                }}

                // Chrome app object
                if (!window.chrome.app) {{
                    window.chrome.app = {{
                        isInstalled: false,
                        InstallState: {{ 
                            DISABLED: 'disabled', 
                            INSTALLED: 'installed', 
                            NOT_INSTALLED: 'not_installed' 
                        }},
                        RunningState: {{ 
                            CANNOT_RUN: 'cannot_run', 
                            READY_TO_RUN: 'ready_to_run', 
                            RUNNING: 'running' 
                        }},
                        getDetails: function() {{ return null; }},
                        getIsInstalled: function() {{ return false; }}
                    }};
                }}
            }})();
        "#,
            ua = self.user_agent(),
            platform = self.os.platform(),
            cores = self.cpu_cores,
            device_memory = self.device_memory_value(),
            webgl_vendor = self.gpu.vendor(),
            webgl_renderer = self.gpu.renderer(),
            chrome_ver = self.chrome_version,
            hints_platform = self.os.hints_platform(),
            platform_version = self.os.platform_version(),
            architecture = self.os.architecture(),
        );

        // Prevent CDP detection via worker threads
        let worker_script = format!(
            r#"
                const OriginalWorker = Worker;
                window.Worker = function (url, options) {{
                
                    const injectedCode = `{script}`
                    const workerPromise = fetch(url)
                        .then((res) => res.text())
                        .then((code) => {{
                            const blob = new Blob([injectedCode + code], {{
                                type: "application/javascript",
                            }});
                            return new OriginalWorker(URL.createObjectURL(blob), options);
                        }});

                    
                        let realWorker = null;
                        const pendingMessages = [];
                        workerPromise.then((w) => {{
                            realWorker = w;
                            pendingMessages.forEach((msg) => w.postMessage(msg));
                        }});
                        return {{
                            postMessage(msg) {{
                            if (realWorker) {{
                                realWorker.postMessage(msg);
                            }} else {{
                                pendingMessages.push(msg);
                            }}
                        }},
                            set onmessage(fn) {{
                                workerPromise.then((w) => (w.onmessage = fn));
                            }},
                            terminate() {{
                                workerPromise.then((w) => w.terminate());
                            }},
                        }};
                }};
            "#,
            script = script
        );

        script.push_str(&worker_script);
        script
    }
}

impl fmt::Display for ChaserProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ChaserProfile({:?}, Chrome {}, {:?})",
            self.os, self.chrome_version, self.gpu
        )
    }
}

/// Builder for constructing `ChaserProfile` instances
#[derive(Debug, Clone)]
pub struct ChaserProfileBuilder {
    os: Os,
    chrome_version: u32,
    gpu: Gpu,
    memory_gb: u32,
    cpu_cores: u32,
    locale: String,
    timezone: String,
    screen_width: u32,
    screen_height: u32,
}

impl ChaserProfileBuilder {
    /// Set the Chrome version (default: 129)
    pub fn chrome_version(mut self, version: u32) -> Self {
        self.chrome_version = version;
        self
    }

    /// Set the GPU for WebGL spoofing
    pub fn gpu(mut self, gpu: Gpu) -> Self {
        self.gpu = gpu;
        self
    }

    /// Set device memory in GB (default: 8)
    pub fn memory_gb(mut self, gb: u32) -> Self {
        self.memory_gb = gb;
        self
    }

    /// Set CPU core count (default: 8)
    pub fn cpu_cores(mut self, cores: u32) -> Self {
        self.cpu_cores = cores;
        self
    }

    /// Set the locale (e.g., "en-US", "de-DE")
    pub fn locale(mut self, locale: impl Into<String>) -> Self {
        self.locale = locale.into();
        self
    }

    /// Set the timezone (e.g., "America/New_York", "Europe/Berlin")
    pub fn timezone(mut self, tz: impl Into<String>) -> Self {
        self.timezone = tz.into();
        self
    }

    /// Set screen resolution
    pub fn screen(mut self, width: u32, height: u32) -> Self {
        self.screen_width = width;
        self.screen_height = height;
        self
    }

    /// Build the final profile
    pub fn build(self) -> ChaserProfile {
        ChaserProfile {
            os: self.os,
            chrome_version: self.chrome_version,
            gpu: self.gpu,
            memory_gb: self.memory_gb,
            cpu_cores: self.cpu_cores,
            locale: self.locale,
            timezone: self.timezone,
            screen_width: self.screen_width,
            screen_height: self.screen_height,
        }
    }
}

/// Detect the host OS at runtime (distinguishes ARM vs Intel on macOS).
pub fn detect_current_os() -> Os {
    #[cfg(target_os = "macos")]
    {
        let is_arm = std::process::Command::new("uname")
            .arg("-m")
            .output()
            .map(|o| {
                let arch = String::from_utf8_lossy(&o.stdout);
                arch.contains("arm64") || arch.contains("aarch64")
            })
            .unwrap_or(false);
        if is_arm { Os::MacOSArm } else { Os::MacOSIntel }
    }
    #[cfg(target_os = "windows")]
    { Os::Windows }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    { Os::Linux }
}

/// Try to detect the installed Chrome major version from the system binary.
/// Returns `None` if Chrome cannot be found or its version parsed.
pub fn detect_chrome_version() -> Option<u32> {
    let path = which::which("google-chrome")
        .or_else(|_| which::which("chromium-browser"))
        .or_else(|_| which::which("chromium"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned());

    #[cfg(target_os = "macos")]
    let path = path.or_else(|| {
        [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
        ]
        .iter()
        .find(|p| std::path::Path::new(p).exists())
        .map(|s| s.to_string())
    });

    let path = path?;
    std::process::Command::new(&path)
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| {
            s.split_whitespace()
                .find(|part| part.starts_with(|c: char| c.is_ascii_digit()))
                .and_then(|v| v.split('.').next())
                .and_then(|major| major.parse().ok())
        })
}

/// Read total system RAM in GB (capped at 8 to match `navigator.deviceMemory` spec max).
pub fn detect_system_memory_gb() -> u32 {
    let gb = _read_system_memory_gb();
    gb.min(8)
}

#[cfg(target_os = "macos")]
fn _read_system_memory_gb() -> u32 {
    std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
        .map(|bytes| (bytes / (1024 * 1024 * 1024)) as u32)
        .unwrap_or(8)
}

#[cfg(target_os = "linux")]
fn _read_system_memory_gb() -> u32 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse::<u64>().ok())
        })
        .map(|kb| (kb / (1024 * 1024)) as u32)
        .unwrap_or(8)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn _read_system_memory_gb() -> u32 {
    8
}

// Re-export the old trait-based system for backwards compatibility
pub use crate::stealth::{LinuxProfile, MacOSProfile, StealthProfile, WindowsNvidiaProfile};
