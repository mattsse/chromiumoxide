use chromiumoxide_cdp::cdp::browser_protocol::emulation::{
    ScreenOrientation, ScreenOrientationType, SetDeviceMetricsOverrideParams,
    SetTouchEmulationEnabledParams,
};
use chromiumoxide_types::Method;

use crate::cmd::CommandChain;
use crate::handler::viewport::Viewport;

#[derive(Debug, Default)]
pub struct EmulationManager {
    pub emulating_mobile: bool,
    pub has_touch: bool,
    pub needs_reload: bool,
}

impl EmulationManager {
    pub fn init_commands(&mut self, viewport: &Option<Viewport>) -> Option<CommandChain> {
        match viewport {
            Some(v) => {
                let orientation = if v.is_landscape {
                    ScreenOrientation::new(ScreenOrientationType::LandscapePrimary, 90)
                } else {
                    ScreenOrientation::new(ScreenOrientationType::PortraitPrimary, 0)
                };

                let set_device = SetDeviceMetricsOverrideParams::builder()
                    .mobile(v.emulating_mobile)
                    .width(v.width)
                    .height(v.height)
                    .device_scale_factor(v.device_scale_factor.unwrap_or(1.))
                    .screen_orientation(orientation)
                    .build()
                    .unwrap();

                let set_touch = SetTouchEmulationEnabledParams::new(true);

                let chain = CommandChain::new(vec![
                    (
                        set_device.identifier(),
                        serde_json::to_value(set_device).unwrap(),
                    ),
                    (
                        set_touch.identifier(),
                        serde_json::to_value(set_touch).unwrap(),
                    ),
                ]);

                self.needs_reload =
                    self.emulating_mobile != v.emulating_mobile || self.has_touch != v.has_touch;
                Some(chain)
            }
            None => None,
        }
    }
}
