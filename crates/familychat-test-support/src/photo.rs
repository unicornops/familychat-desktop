//! A photo with a location in it, for the tests that prove a picture's
//! location never leaves the device (ADR 0012): a 32x24 JPEG (made with
//! ImageMagick, `testdata/photo.jpg`) with an EXIF block holding a GPS
//! position and an XMP packet saying the same, as a phone writes them.

/// The photo without any metadata.
pub const PLAIN_PHOTO: &[u8] = include_bytes!("testdata/photo.jpg");

/// The latitude the photo's metadata gives, as its XMP writes it.
pub const PHOTO_LATITUDE: &str = "53,20.5N";

/// A TIFF structure (little-endian) with an orientation and a GPS block
/// holding a latitude reference.
fn exif() -> Vec<u8> {
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    tiff.extend_from_slice(&2u16.to_le_bytes());
    // Orientation: upright.
    tiff.extend_from_slice(&0x0112u16.to_le_bytes());
    tiff.extend_from_slice(&3u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&[0, 0]);
    // The GPS IFD, right after this one.
    tiff.extend_from_slice(&0x8825u16.to_le_bytes());
    tiff.extend_from_slice(&4u16.to_le_bytes());
    tiff.extend_from_slice(&1u32.to_le_bytes());
    tiff.extend_from_slice(&38u32.to_le_bytes());
    tiff.extend_from_slice(&0u32.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&1u16.to_le_bytes());
    tiff.extend_from_slice(&2u16.to_le_bytes());
    tiff.extend_from_slice(&2u32.to_le_bytes());
    tiff.extend_from_slice(b"N\0\0\0");
    tiff.extend_from_slice(&0u32.to_le_bytes());
    let mut segment = b"Exif\0\0".to_vec();
    segment.extend_from_slice(&tiff);
    segment
}

fn app1(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0xFF, 0xE1];
    let length = u16::try_from(data.len() + 2).unwrap_or(u16::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(data);
    out
}

/// The photo with a GPS position in its EXIF and its XMP.
pub fn photo_with_location() -> Vec<u8> {
    let mut xmp = b"http://ns.adobe.com/xap/1.0/\0".to_vec();
    xmp.extend_from_slice(
        format!(
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description \
             exif:GPSLatitude=\"{PHOTO_LATITUDE}\" exif:GPSLongitude=\"6,15.6W\"/>\
             </rdf:RDF></x:xmpmeta>"
        )
        .as_bytes(),
    );
    let mut out = PLAIN_PHOTO[..2].to_vec();
    out.extend(app1(&exif()));
    out.extend(app1(&xmp));
    out.extend_from_slice(&PLAIN_PHOTO[2..]);
    out
}
