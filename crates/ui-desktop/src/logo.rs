//! The Ruby logo, a faceted red gem with an R cut into it, from the image the user supplied. Shared with the launcher, which writes
//! it as an `.ico` for its shortcuts, so this file depends on nothing.

/// The logo's straight-alpha RGBA pixels at each size, made from the source image by
/// `scripts/make-logo.py`.
const BLOBS: [(usize, &[u8]); 4] = [
   (16, include_bytes!("logo/16.rgba")),
   (32, include_bytes!("logo/32.rgba")),
   (48, include_bytes!("logo/48.rgba")),
   (64, include_bytes!("logo/64.rgba")),
];

/// The logo at `size` pixels square: `0xAARRGGBB` pixels, top row first. A size without a blob is
/// the largest one sampled down by nearest neighbour.
pub fn pixels(size: usize) -> Vec<u32> {
   let (from, rgba) = BLOBS
      .iter()
      .find(|(known, _)| *known == size)
      .copied()
      .unwrap_or(BLOBS[BLOBS.len() - 1]);
   (0..size * size)
      .map(|i| {
         let (x, y) = (i % size * from / size, i / size * from / size);
         let at = (y * from + x) * 4;
         let [r, g, b, a] = [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]].map(u32::from);
         a << 24 | r << 16 | g << 8 | b
      })
      .collect()
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
   fn has_a_solid_logo_on_a_clear_background() {
      for size in [16, 32, 48, 64, 24] {
         let image = pixels(size);
         assert_eq!(image.len(), size * size);
         assert_eq!(image[0] >> 24, 0, "{size}: the top left corner is clear");
         let drawn = image.iter().filter(|&&p| p >> 24 != 0).count();
         assert!(
            drawn > size * size / 4,
            "{size}: the logo fills a good part of the square"
         );
         assert!(
            image.iter().any(|&p| p >> 24 >= 250),
            "{size}: something is solid"
         );
      }
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
