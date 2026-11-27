//! The Ruby logo, a cut ruby seen from the side, drawn from a few flat facets so it needs no
//! image file. Shared with the launcher, which writes it as an `.ico` for its shortcuts, so this
//! file depends on nothing.

type Point = (f32, f32);

/// Facets in `0.0..=1.0` square coordinates, painted in order, so later ones lie on top.
const FACETS: &[(&[Point], u32)] = &[
   // Crown: the table and its two sides.
   (
      &[(0.30, 0.14), (0.70, 0.14), (0.60, 0.32), (0.40, 0.32)],
      0xFFFF_6B81,
   ),
   (&[(0.14, 0.32), (0.30, 0.14), (0.40, 0.32)], 0xFFE0_294F),
   (&[(0.86, 0.32), (0.70, 0.14), (0.60, 0.32)], 0xFFC2_1A40),
   // Pavilion, down to the point.
   (&[(0.14, 0.32), (0.40, 0.32), (0.50, 0.90)], 0xFFC4_153F),
   (&[(0.40, 0.32), (0.60, 0.32), (0.50, 0.90)], 0xFFE8_325A),
   (&[(0.60, 0.32), (0.86, 0.32), (0.50, 0.90)], 0xFF92_102F),
   // Glint on the table.
   (
      &[(0.38, 0.18), (0.50, 0.18), (0.46, 0.27), (0.40, 0.27)],
      0xFFFF_B3C0,
   ),
];

/// Whether `p` lies inside the convex polygon `poly`.
fn inside(poly: &[Point], p: Point) -> bool {
   let mut sign = 0.0_f32;
   for (i, &(ax, ay)) in poly.iter().enumerate() {
      let (bx, by) = poly[(i + 1) % poly.len()];
      let cross = (bx - ax) * (p.1 - ay) - (by - ay) * (p.0 - ax);
      if cross != 0.0 {
         if sign != 0.0 && (cross > 0.0) != (sign > 0.0) {
            return false;
         }
         sign = cross;
      }
   }
   true
}

/// The logo at `size` pixels square: `0xAARRGGBB` pixels, top row first, edges smoothed by 4×4
/// samples.
pub fn pixels(size: usize) -> Vec<u32> {
   const SAMPLES: usize = 4;
   let mut out = Vec::with_capacity(size * size);
   for y in 0..size {
      for x in 0..size {
         let (mut covered, mut r, mut g, mut b) = (0_u32, 0_u32, 0_u32, 0_u32);
         for sy in 0..SAMPLES {
            for sx in 0..SAMPLES {
               let p = (
                  (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / size as f32,
                  (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / size as f32,
               );
               if let Some(&(_, color)) = FACETS.iter().rev().find(|(poly, _)| inside(poly, p)) {
                  covered += 1;
                  r += (color >> 16) & 0xFF;
                  g += (color >> 8) & 0xFF;
                  b += color & 0xFF;
               }
            }
         }
         out.push(if covered == 0 {
            0
         } else {
            let alpha = covered * 255 / (SAMPLES * SAMPLES) as u32;
            alpha << 24 | (r / covered) << 16 | (g / covered) << 8 | b / covered
         });
      }
   }
   out
}

/// An `.ico` holding the logo at each of `sizes` (at most 255 pixels).
pub fn ico(sizes: &[usize]) -> Vec<u8> {
   let images: Vec<Vec<u8>> = sizes
      .iter()
      .map(|&size| {
         let mut image = Vec::new();
         // BITMAPINFOHEADER: the height counts the colour rows and the mask rows.
         for field in [40_u32, size as u32, size as u32 * 2] {
            image.extend(field.to_le_bytes());
         }
         image.extend(1_u16.to_le_bytes());
         image.extend(32_u16.to_le_bytes());
         image.extend([0; 24]);
         // Pixels bottom row first, as BGRA, then a 1-bit mask (all zero: the alpha decides).
         let pixels = pixels(size);
         for row in pixels.chunks(size).rev() {
            for &pixel in row {
               image.extend(pixel.to_le_bytes());
            }
         }
         image.extend(vec![0; size.div_ceil(32) * 4 * size]);
         image
      })
      .collect();
   let mut ico = Vec::new();
   ico.extend([0, 0, 1, 0]);
   ico.extend((sizes.len() as u16).to_le_bytes());
   let mut offset = 6 + 16 * sizes.len();
   for (&size, image) in sizes.iter().zip(&images) {
      ico.extend([size as u8, size as u8, 0, 0, 1, 0, 32, 0]);
      ico.extend((image.len() as u32).to_le_bytes());
      ico.extend((offset as u32).to_le_bytes());
      offset += image.len();
   }
   images.into_iter().for_each(|image| ico.extend(image));
   ico
}

#[cfg(test)]
mod tests {
   use super::*;

   #[test]
   fn draws_a_gem_with_clear_corners() {
      let image = pixels(32);
      assert_eq!(image.len(), 32 * 32);
      assert_eq!(image[0], 0, "the top left corner is outside the gem");
      assert_eq!(image[31 * 32], 0, "so is the bottom left");
      let middle = image[12 * 32 + 16];
      assert_eq!(middle >> 24, 255, "the middle is solid");
      assert!(
         image.iter().any(|&p| (1..255).contains(&(p >> 24))),
         "edges are smoothed"
      );
   }

   #[test]
   fn ico_has_a_directory_entry_per_size() {
      let ico = ico(&[16, 32]);
      assert_eq!(&ico[..6], [0, 0, 1, 0, 2, 0]);
      // The second image starts where the first entry says it ends.
      let first_len = u32::from_le_bytes(ico[14..18].try_into().unwrap()) as usize;
      let first_at = u32::from_le_bytes(ico[18..22].try_into().unwrap()) as usize;
      let second_at = u32::from_le_bytes(ico[34..38].try_into().unwrap()) as usize;
      assert_eq!(first_at + first_len, second_at);
      assert_eq!(first_len, 40 + 16 * 16 * 4 + 16 * 4);
   }
}
