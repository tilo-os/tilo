//! Optional presentation hints. Unsupported backends keep full presentation.

/// An outward-rounded rectangle in framebuffer pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Hint which pixels differ from the last presented buffer. An empty hint
/// preserves whole-surface presentation; it never permits partial buffer copies.
#[cfg(all(feature = "gpu-damage", not(target_arch = "wasm32")))]
pub fn set_next_present_damage(surface: &wgpu::Surface<'_>, damage: &[DamageRect]) -> bool {
    #[cfg(target_os = "linux")]
    {
        let regions: Vec<_> = damage
            .iter()
            .map(|rect| wgpu::hal::vulkan::PresentDamageRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect();
        // SAFETY: borrow only this configured surface's HAL guard and use its
        // checked hint setter. No handle is retained or destroyed, and the guard
        // is dropped before the caller presents and takes the swapchain locks.
        if let Some(hal) = unsafe { surface.as_hal::<wgpu::hal::vulkan::Api>() } {
            hal.set_next_present_damage(&regions);
            return true;
        }
    }
    let _ = (surface, damage);
    false
}
