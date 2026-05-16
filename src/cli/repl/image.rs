//! Image file loading and base64 encoding.

use anyhow::Result;

/// Load an image file and return its base64-encoded content with MIME type.
pub(crate) fn load_image_as_base64(path: &str) -> Result<(String, String)> {
    use std::path::Path;
    use anyhow::Context as _;

    let path = Path::new(path);
    if !path.exists() {
        anyhow::bail!("File not found: {}", path.display());
    }

    let extension = path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    let media_type = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => anyhow::bail!("Unsupported image format: .{}. Supported: png, jpg, gif, webp, svg", extension),
    };

    let data = std::fs::read(path)
        .with_context(|| format!("Failed to read image file: {}", path.display()))?;

    // Check file size (max 20MB for most APIs)
    if data.len() > 20 * 1024 * 1024 {
        anyhow::bail!("Image file too large ({} bytes). Maximum: 20MB", data.len());
    }

    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&data);

    Ok((media_type.to_string(), encoded))
}
