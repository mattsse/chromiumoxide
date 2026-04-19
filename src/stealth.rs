//! Stealth profile system for customizable browser fingerprints.
//!
//! This module provides a trait-based system for defining browser "personalities"
//! that can bypass anti-bot detection. The community can contribute new profiles
//! as Chrome versions and GPU models evolve.

/// A trait for defining a consistent browser fingerprint profile.
///
/// Implementors define all the values that make up a coherent browser identity.
/// The key insight is that all values must be **internally consistent** -
/// a Windows User-Agent with a MacOS platform is immediately flagged as suspicious.
///
/// # Example
///
/// ```rust
/// use chaser_oxide::stealth::StealthProfile;
///
/// struct LinuxChromeProfile;
///
/// impl StealthProfile for LinuxChromeProfile {
///     fn user_agent(&self) -> &str {
///         "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/129.0.0.0"
///     }
///     fn platform(&self) -> &str { "Linux x86_64" }
///     fn webgl_vendor(&self) -> &str { "Google Inc. (Intel)" }
///     fn webgl_renderer(&self) -> &str {
///         "ANGLE (Intel, Intel UHD Graphics 630 Direct3D11)"
///     }
///     fn hardware_concurrency(&self) -> u32 { 4 }
///     fn device_memory(&self) -> u32 { 8 }
/// }
/// ```
pub trait StealthProfile: Send + Sync {
    /// The User-Agent string (must match platform/hardware)
    fn user_agent(&self) -> &str;

    /// The `navigator.platform` value (e.g., "Win32", "MacIntel", "Linux x86_64")
    fn platform(&self) -> &str;

    /// The WebGL UNMASKED_VENDOR_WEBGL value
    fn webgl_vendor(&self) -> &str;

    /// The WebGL UNMASKED_RENDERER_WEBGL value
    fn webgl_renderer(&self) -> &str;

    /// The `navigator.hardwareConcurrency` value (CPU threads)
    fn hardware_concurrency(&self) -> u32;

    /// The `navigator.deviceMemory` value (RAM in GB)
    fn device_memory(&self) -> u32;

    /// Client hints brands array
    fn client_hints_brands(&self) -> Vec<(&str, &str)> {
        vec![
            ("Google Chrome", "129"),
            ("Chromium", "129"),
            ("Not=A?Brand", "24"),
        ]
    }

    /// Client hints platform
    fn client_hints_platform(&self) -> &str {
        "Windows"
    }

    /// Generate the complete JavaScript bootstrap script
    fn bootstrap_script(&self) -> String {
        format!(
            r#"
            // === chaser-oxide HARDWARE HARMONY ===
            // Profile: {ua}

            // 1. Platform alignment (on prototype to avoid getOwnPropertyNames detection)
            Object.defineProperty(Navigator.prototype, 'platform', {{
                get: () => '{platform}',
                configurable: true
            }});

            // 2. Hardware specs (on prototype)
            Object.defineProperty(Navigator.prototype, 'hardwareConcurrency', {{
                get: () => {cores},
                configurable: true
            }});
            Object.defineProperty(Navigator.prototype, 'deviceMemory', {{
                get: () => {memory},
                configurable: true
            }});

            // 3. WebGL spoofing (both contexts)
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
                    brands: [{brands}],
                    mobile: false,
                    platform: "{hints_platform}"
                }}),
                configurable: true
            }});

            // 5. Video codecs (H.264/AAC)
            const canPlayType = HTMLMediaElement.prototype.canPlayType;
            HTMLMediaElement.prototype.canPlayType = function(type) {{
                if (type.includes('avc1')) return 'probably';
                if (type.includes('mp4a.40')) return 'probably';
                if (type === 'video/mp4') return 'probably';
                if (type === 'audio/mp4') return 'probably';
                return canPlayType.apply(this, arguments);
            }};

            // 6. WebDriver - set to false (not delete, which makes it undefined)
            Object.defineProperty(Object.getPrototypeOf(navigator), 'webdriver', {{
                get: () => false,
                configurable: true
            }});

            // 7. window.chrome
            window.chrome = {{ runtime: {{}} }};
        "#,
            ua = self.user_agent(),
            platform = self.platform(),
            cores = self.hardware_concurrency(),
            memory = self.device_memory(),
            webgl_vendor = self.webgl_vendor(),
            webgl_renderer = self.webgl_renderer(),
            brands = self
                .client_hints_brands()
                .iter()
                .map(|(b, v)| format!(r#"{{ brand: "{}", version: "{}" }}"#, b, v))
                .collect::<Vec<_>>()
                .join(", "),
            hints_platform = self.client_hints_platform(),
        )
    }
}

/// The default "Windows Gamer" profile - high trust, common configuration.
///
/// This profile represents a typical Windows 10/11 user with an NVIDIA RTX GPU,
/// which is one of the most common and trusted browser configurations globally.
#[derive(Debug, Clone, Default)]
pub struct WindowsNvidiaProfile;

impl StealthProfile for WindowsNvidiaProfile {
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"
    }

    fn platform(&self) -> &str {
        "Win32"
    }

    fn webgl_vendor(&self) -> &str {
        "Google Inc. (NVIDIA)"
    }

    fn webgl_renderer(&self) -> &str {
        "ANGLE (NVIDIA, NVIDIA GeForce RTX 3080 Direct3D11 vs_5_0 ps_5_0)"
    }

    fn hardware_concurrency(&self) -> u32 {
        8
    }

    fn device_memory(&self) -> u32 {
        8
    }
}

/// A MacOS profile for users who need to appear as Mac users.
#[derive(Debug, Clone, Default)]
pub struct MacOSProfile;

impl StealthProfile for MacOSProfile {
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"
    }

    fn platform(&self) -> &str {
        "MacIntel"
    }

    fn webgl_vendor(&self) -> &str {
        "Google Inc. (Apple)"
    }

    fn webgl_renderer(&self) -> &str {
        "ANGLE (Apple, Apple M1 Pro, OpenGL 4.1)"
    }

    fn hardware_concurrency(&self) -> u32 {
        10
    }

    fn device_memory(&self) -> u32 {
        16
    }

    fn client_hints_platform(&self) -> &str {
        "macOS"
    }
}

/// A Linux profile for users who need to appear as Linux users.
#[derive(Debug, Clone, Default)]
pub struct LinuxProfile;

impl StealthProfile for LinuxProfile {
    fn user_agent(&self) -> &str {
        "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Safari/537.36"
    }

    fn platform(&self) -> &str {
        "Linux x86_64"
    }

    fn webgl_vendor(&self) -> &str {
        "Google Inc. (NVIDIA Corporation)"
    }

    fn webgl_renderer(&self) -> &str {
        "NVIDIA GeForce GTX 1080/PCIe/SSE2"
    }

    fn hardware_concurrency(&self) -> u32 {
        8
    }

    fn device_memory(&self) -> u32 {
        16
    }

    fn client_hints_platform(&self) -> &str {
        "Linux"
    }
}
