//! Client-side cursor rendering for screen streaming.
//!
//! This module handles rendering the remote cursor on top of received video frames.
//! The cursor is rendered client-side for lower latency, as cursor updates can be
//! sent more frequently than video frames.

use crate::iroh::protocol::{CursorFormat, CursorShape as ProtoCursorShape, CursorUpdate};
use std::collections::HashMap;
use tracing::debug;

/// Maximum number of cursor shapes to cache.
const MAX_CACHED_SHAPES: usize = 32;

/// Client-side cursor renderer.
///
/// Maintains a cache of cursor shapes and renders them onto video frames.
#[derive(Debug)]
pub struct CursorRenderer {
    /// Current cursor position (screen coordinates)
    position: (i32, i32),
    /// Whether cursor is visible
    visible: bool,
    /// Current shape ID
    current_shape_id: u64,
    /// Cache of cursor shapes (RGBA data)
    shape_cache: HashMap<u64, CachedShape>,
    /// LRU order for cache eviction
    shape_order: Vec<u64>,
}

/// Cached cursor shape ready for rendering.
#[derive(Debug, Clone)]
struct CachedShape {
    /// Width in pixels
    width: u32,
    /// Height in pixels
    height: u32,
    /// Hotspot X offset
    hotspot_x: u32,
    /// Hotspot Y offset
    hotspot_y: u32,
    /// RGBA pixel data
    data: Vec<u8>,
}

impl Default for CursorRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl CursorRenderer {
    /// Create a new cursor renderer.
    pub fn new() -> Self {
        Self {
            position: (0, 0),
            visible: false,
            current_shape_id: 0,
            shape_cache: HashMap::new(),
            shape_order: Vec::new(),
        }
    }

    /// Update cursor state from a cursor update message.
    pub fn update(&mut self, update: &CursorUpdate) {
        self.position = (update.x, update.y);
        self.visible = update.visible;

        // Cache new shape if provided
        if let Some(ref shape) = update.shape {
            self.cache_shape(shape);
            self.current_shape_id = shape.shape_id;
        }
    }

    /// Cache a cursor shape, converting from protocol format to RGBA.
    fn cache_shape(&mut self, shape: &ProtoCursorShape) {
        // Check if already cached
        if self.shape_cache.contains_key(&shape.shape_id) {
            // Move to front of LRU
            self.shape_order.retain(|&id| id != shape.shape_id);
            self.shape_order.push(shape.shape_id);
            return;
        }

        // Convert to RGBA if needed
        let rgba_data = match shape.format {
            CursorFormat::Rgba32 => shape.data.clone(),
            CursorFormat::Monochrome => self.mono_to_rgba(&shape.data, shape.width, shape.height),
            CursorFormat::MaskedColor => {
                // MaskedColor should already be RGBA from our capture
                shape.data.clone()
            }
        };

        let cached = CachedShape {
            width: shape.width,
            height: shape.height,
            hotspot_x: shape.hotspot_x,
            hotspot_y: shape.hotspot_y,
            data: rgba_data,
        };

        // Evict oldest if cache is full
        while self.shape_cache.len() >= MAX_CACHED_SHAPES && !self.shape_order.is_empty() {
            let oldest = self.shape_order.remove(0);
            self.shape_cache.remove(&oldest);
            debug!("Evicted cursor shape {} from cache", oldest);
        }

        self.shape_cache.insert(shape.shape_id, cached);
        self.shape_order.push(shape.shape_id);
        debug!(
            "Cached cursor shape {}: {}x{}",
            shape.shape_id, shape.width, shape.height
        );
    }

    /// Convert monochrome cursor to RGBA.
    fn mono_to_rgba(&self, _data: &[u8], _width: u32, _height: u32) -> Vec<u8> {
        // Monochrome conversion is already done on the capture side
        // This is a fallback that shouldn't normally be used
        Vec::new()
    }

    /// Render cursor onto a frame.
    ///
    /// # Arguments
    /// * `frame` - Mutable RGBA frame data (modified in place)
    /// * `frame_width` - Frame width in pixels
    /// * `frame_height` - Frame height in pixels
    pub fn render_on_frame(&self, frame: &mut [u8], frame_width: u32, frame_height: u32) {
        if !self.visible {
            return;
        }

        let shape = match self.shape_cache.get(&self.current_shape_id) {
            Some(s) => s,
            None => return,
        };

        // Calculate render position (subtract hotspot)
        let render_x = self.position.0 - shape.hotspot_x as i32;
        let render_y = self.position.1 - shape.hotspot_y as i32;

        // Render cursor with alpha blending
        for cy in 0..shape.height {
            let frame_y = render_y + cy as i32;
            if frame_y < 0 || frame_y >= frame_height as i32 {
                continue;
            }

            for cx in 0..shape.width {
                let frame_x = render_x + cx as i32;
                if frame_x < 0 || frame_x >= frame_width as i32 {
                    continue;
                }

                // Get cursor pixel
                let cursor_idx = ((cy * shape.width + cx) * 4) as usize;
                if cursor_idx + 3 >= shape.data.len() {
                    continue;
                }

                let src_r = shape.data[cursor_idx];
                let src_g = shape.data[cursor_idx + 1];
                let src_b = shape.data[cursor_idx + 2];
                let src_a = shape.data[cursor_idx + 3];

                // Skip fully transparent pixels
                if src_a == 0 {
                    continue;
                }

                // Get frame pixel
                let frame_idx = ((frame_y as u32 * frame_width + frame_x as u32) * 4) as usize;
                if frame_idx + 3 >= frame.len() {
                    continue;
                }

                // Alpha blend
                if src_a == 255 {
                    // Fully opaque - direct copy
                    frame[frame_idx] = src_r;
                    frame[frame_idx + 1] = src_g;
                    frame[frame_idx + 2] = src_b;
                } else {
                    // Partial transparency - blend
                    let dst_r = frame[frame_idx];
                    let dst_g = frame[frame_idx + 1];
                    let dst_b = frame[frame_idx + 2];

                    let alpha = src_a as u16;
                    let inv_alpha = 255 - alpha;

                    frame[frame_idx] =
                        ((src_r as u16 * alpha + dst_r as u16 * inv_alpha) / 255) as u8;
                    frame[frame_idx + 1] =
                        ((src_g as u16 * alpha + dst_g as u16 * inv_alpha) / 255) as u8;
                    frame[frame_idx + 2] =
                        ((src_b as u16 * alpha + dst_b as u16 * inv_alpha) / 255) as u8;
                }
            }
        }
    }

    /// Get current cursor position.
    pub fn position(&self) -> (i32, i32) {
        self.position
    }

    /// Check if cursor is visible.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Set cursor visibility.
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    /// Clear the shape cache.
    pub fn clear_cache(&mut self) {
        self.shape_cache.clear();
        self.shape_order.clear();
        self.current_shape_id = 0;
    }

    /// Get number of cached shapes.
    pub fn cached_shape_count(&self) -> usize {
        self.shape_cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cursor_renderer_creation() {
        let renderer = CursorRenderer::new();
        assert!(!renderer.is_visible());
        assert_eq!(renderer.position(), (0, 0));
        assert_eq!(renderer.cached_shape_count(), 0);
    }

    #[test]
    fn test_cursor_update() {
        let mut renderer = CursorRenderer::new();

        // Create a simple 2x2 white cursor
        let shape = ProtoCursorShape {
            shape_id: 1,
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            format: CursorFormat::Rgba32,
            data: vec![255; 16], // 2x2x4 = 16 bytes, all white
        };

        let update = CursorUpdate {
            x: 100,
            y: 200,
            visible: true,
            shape: Some(shape),
        };

        renderer.update(&update);

        assert!(renderer.is_visible());
        assert_eq!(renderer.position(), (100, 200));
        assert_eq!(renderer.cached_shape_count(), 1);
    }

    #[test]
    fn test_cursor_render() {
        let mut renderer = CursorRenderer::new();

        // Create a 2x2 red cursor
        let shape = ProtoCursorShape {
            shape_id: 1,
            width: 2,
            height: 2,
            hotspot_x: 0,
            hotspot_y: 0,
            format: CursorFormat::Rgba32,
            data: vec![
                255, 0, 0, 255, // Red pixel
                255, 0, 0, 255, // Red pixel
                255, 0, 0, 255, // Red pixel
                255, 0, 0, 255, // Red pixel
            ],
        };

        let update = CursorUpdate {
            x: 1,
            y: 1,
            visible: true,
            shape: Some(shape),
        };
        renderer.update(&update);

        // Create a 4x4 green frame
        let mut frame = vec![
            0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, // Row 0
            0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, // Row 1
            0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, // Row 2
            0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, // Row 3
        ];

        renderer.render_on_frame(&mut frame, 4, 4);

        // Check that pixels at (1,1), (2,1), (1,2), (2,2) are now red
        // Pixel (1,1) = index 4*1 + 1 = 5, byte offset = 5*4 = 20
        assert_eq!(frame[20], 255); // R
        assert_eq!(frame[21], 0); // G
        assert_eq!(frame[22], 0); // B
    }

    #[test]
    fn test_cache_eviction() {
        let mut renderer = CursorRenderer::new();

        // Add MAX_CACHED_SHAPES + 5 shapes
        for i in 0..(MAX_CACHED_SHAPES + 5) as u64 {
            let shape = ProtoCursorShape {
                shape_id: i,
                width: 1,
                height: 1,
                hotspot_x: 0,
                hotspot_y: 0,
                format: CursorFormat::Rgba32,
                data: vec![255, 255, 255, 255],
            };

            let update = CursorUpdate {
                x: 0,
                y: 0,
                visible: true,
                shape: Some(shape),
            };
            renderer.update(&update);
        }

        // Cache should be at MAX_CACHED_SHAPES
        assert_eq!(renderer.cached_shape_count(), MAX_CACHED_SHAPES);
    }
}
