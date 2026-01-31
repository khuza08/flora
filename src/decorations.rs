use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::utils::{Size, Transform};

/// Generate a circle image as RGBA data
pub fn generate_circle_rgba(size: u32, color: [u8; 4]) -> Vec<u8> {
    let mut data = vec![0u8; (size * size * 4) as usize];
    let center = size as f32 / 2.0;
    let radius = center - 0.5; // Slight inset for anti-aliasing edge
    
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center + 0.5;
            let dy = y as f32 - center + 0.5;
            let distance = (dx * dx + dy * dy).sqrt();
            
            let idx = ((y * size + x) * 4) as usize;
            
            if distance <= radius {
                // Inside the circle
                data[idx] = color[0];     // R
                data[idx + 1] = color[1]; // G
                data[idx + 2] = color[2]; // B
                data[idx + 3] = color[3]; // A (opaque)
            } else if distance <= radius + 1.0 {
                // Anti-aliased edge
                let alpha = ((radius + 1.0 - distance) * color[3] as f32) as u8;
                data[idx] = color[0];
                data[idx + 1] = color[1];
                data[idx + 2] = color[2];
                data[idx + 3] = alpha;
            }
            // else: transparent (already 0)
        }
    }
    data
}

/// Create a MemoryRenderBuffer for a circle button
pub fn create_circle_buffer(size: i32, color: [u8; 4]) -> MemoryRenderBuffer {
    let rgba_data = generate_circle_rgba(size as u32, color);
    
    MemoryRenderBuffer::from_slice(
        &rgba_data,
        smithay::backend::allocator::Fourcc::Abgr8888, // RGBA in memory = ABGR in fourcc
        Size::from((size, size)),
        1, // scale
        Transform::Normal,
        None,
    )
}

/// Colors for macOS-style traffic light buttons (RGBA)
pub const RED_BUTTON_COLOR: [u8; 4] = [255, 95, 87, 255];    // Close
pub const YELLOW_BUTTON_COLOR: [u8; 4] = [255, 189, 46, 255]; // Minimize  
pub const GREEN_BUTTON_COLOR: [u8; 4] = [40, 200, 64, 255];   // Maximize
