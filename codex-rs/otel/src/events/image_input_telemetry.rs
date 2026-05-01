use codex_protocol::user_input::UserInput;
use serde::Serialize;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

const IMAGE_HEADER_READ_LIMIT: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageInputTelemetry {
    pub(crate) details_json: String,
    pub(crate) image_types: String,
    pub(crate) mime_types: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct ImageInputDetail {
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    byte_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extension: Option<String>,
}

pub(crate) fn image_input_telemetry(items: &[UserInput]) -> Option<ImageInputTelemetry> {
    let details: Vec<ImageInputDetail> = items
        .iter()
        .filter_map(|item| match item {
            UserInput::Image { image_url } => Some(remote_or_data_url_detail(image_url)),
            UserInput::LocalImage { path } => Some(local_image_detail(path)),
            UserInput::Text { .. } | UserInput::Skill { .. } | UserInput::Mention { .. } => None,
            _ => None,
        })
        .collect();

    if details.is_empty() {
        return None;
    }

    let image_types = comma_join(
        details
            .iter()
            .filter_map(|detail| detail.image_type.as_deref()),
    );
    let mime_types = comma_join(
        details
            .iter()
            .filter_map(|detail| detail.mime_type.as_deref()),
    );
    let details_json = serde_json::to_string(&details).ok()?;

    Some(ImageInputTelemetry {
        details_json,
        image_types,
        mime_types,
    })
}

fn remote_or_data_url_detail(image_url: &str) -> ImageInputDetail {
    if let Some(detail) = data_url_detail(image_url) {
        return detail;
    }

    let extension = safe_image_extension_from_url(image_url);
    let mime_type = extension.as_deref().and_then(mime_type_from_extension);
    ImageInputDetail {
        source: "remote_url",
        image_type: mime_type.as_deref().and_then(image_type_from_mime),
        mime_type,
        width: None,
        height: None,
        byte_length: None,
        extension,
    }
}

fn data_url_detail(image_url: &str) -> Option<ImageInputDetail> {
    let image_url = strip_data_scheme(image_url)?;
    let (metadata, payload) = image_url.split_once(',')?;
    let mut metadata_parts = metadata.split(';');
    let mime_type = metadata_parts.next()?.to_ascii_lowercase();
    if !mime_type.starts_with("image/") {
        return None;
    }
    let is_base64 = metadata_parts.any(|part| part.eq_ignore_ascii_case("base64"));

    Some(ImageInputDetail {
        source: "data_url",
        image_type: image_type_from_mime(&mime_type),
        mime_type: Some(mime_type),
        width: None,
        height: None,
        byte_length: is_base64.then(|| base64_payload_byte_len(payload)),
        extension: None,
    })
}

fn local_image_detail(path: &Path) -> ImageInputDetail {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|extension| mime_type_from_extension(extension).is_some());

    let byte_length = path.metadata().ok().map(|metadata| metadata.len());
    let header = read_image_header(path).unwrap_or_default();
    let header_info = image_info_from_header(&header);
    let mime_type = header_info
        .as_ref()
        .map(|info| info.mime_type.to_string())
        .or_else(|| extension.as_deref().and_then(mime_type_from_extension));

    ImageInputDetail {
        source: "local_file",
        image_type: mime_type
            .as_deref()
            .and_then(image_type_from_mime)
            .or(extension.clone()),
        mime_type,
        width: header_info.as_ref().and_then(|info| info.width),
        height: header_info.as_ref().and_then(|info| info.height),
        byte_length,
        extension,
    }
}

fn strip_data_scheme(image_url: &str) -> Option<&str> {
    let scheme = image_url.get(..5)?;
    if !scheme.eq_ignore_ascii_case("data:") {
        return None;
    }
    image_url.get(5..)
}

fn read_image_header(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut header = Vec::new();
    file.take(IMAGE_HEADER_READ_LIMIT)
        .read_to_end(&mut header)?;
    Ok(header)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeaderImageInfo {
    mime_type: &'static str,
    width: Option<u32>,
    height: Option<u32>,
}

fn image_info_from_header(bytes: &[u8]) -> Option<HeaderImageInfo> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") && bytes.len() >= 24 {
        return Some(HeaderImageInfo {
            mime_type: "image/png",
            width: Some(u32::from_be_bytes(bytes[16..20].try_into().ok()?)),
            height: Some(u32::from_be_bytes(bytes[20..24].try_into().ok()?)),
        });
    }

    if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) && bytes.len() >= 10 {
        return Some(HeaderImageInfo {
            mime_type: "image/gif",
            width: Some(u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32),
            height: Some(u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32),
        });
    }

    if bytes.starts_with(b"\xff\xd8") {
        return jpeg_info_from_header(bytes);
    }

    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP" {
        return Some(HeaderImageInfo {
            mime_type: "image/webp",
            width: None,
            height: None,
        });
    }

    None
}

fn jpeg_info_from_header(bytes: &[u8]) -> Option<HeaderImageInfo> {
    let mut i = 2;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xff {
            i += 1;
            continue;
        }
        while i < bytes.len() && bytes[i] == 0xff {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        let marker = bytes[i];
        i += 1;
        if marker == 0xd9 || marker == 0xda {
            break;
        }
        if i + 2 > bytes.len() {
            break;
        }

        let segment_len = u16::from_be_bytes(bytes[i..i + 2].try_into().ok()?) as usize;
        if segment_len < 2 || i + segment_len > bytes.len() {
            break;
        }
        if is_jpeg_start_of_frame(marker) && segment_len >= 7 {
            return Some(HeaderImageInfo {
                mime_type: "image/jpeg",
                width: Some(u16::from_be_bytes(bytes[i + 5..i + 7].try_into().ok()?) as u32),
                height: Some(u16::from_be_bytes(bytes[i + 3..i + 5].try_into().ok()?) as u32),
            });
        }
        i += segment_len;
    }

    Some(HeaderImageInfo {
        mime_type: "image/jpeg",
        width: None,
        height: None,
    })
}

fn is_jpeg_start_of_frame(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

fn safe_image_extension_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)?;
    mime_type_from_extension(&extension)?;
    Some(extension)
}

fn mime_type_from_extension(extension: &str) -> Option<String> {
    match extension {
        "png" => Some("image/png".to_string()),
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "gif" => Some("image/gif".to_string()),
        "webp" => Some("image/webp".to_string()),
        "bmp" => Some("image/bmp".to_string()),
        "heic" => Some("image/heic".to_string()),
        "heif" => Some("image/heif".to_string()),
        "tif" | "tiff" => Some("image/tiff".to_string()),
        "svg" => Some("image/svg+xml".to_string()),
        _ => None,
    }
}

fn image_type_from_mime(mime_type: &str) -> Option<String> {
    mime_type
        .strip_prefix("image/")
        .map(str::to_ascii_lowercase)
}

fn base64_payload_byte_len(payload: &str) -> u64 {
    let trimmed = payload.trim_end_matches('=');
    ((trimmed.len() * 3) / 4) as u64
}

fn comma_join<'a>(values: impl Iterator<Item = &'a str>) -> String {
    values
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",")
}
