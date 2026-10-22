//! SVG export: document to valid SVG and SVGZ XML.

use std::fmt::Write as _;
use std::io::Write as _;

use photocraft_color::blend::BlendMode;
use photocraft_color::Color;
use photocraft_doc::effects::GradientStyle;
use photocraft_doc::vector::{FillRule, LineCap, LineJoin, Path, ShapeStroke};
use photocraft_doc::{Document, Fill, Layer, LayerContent, MAX_GROUP_DEPTH};
use photocraft_geom::Affine;

use crate::{ExportResult, IoError};

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn svg_color(c: &Color) -> (String, f32) {
    let [r, g, b, a] = c.to_rgba8();
    (format!("rgb({r},{g},{b})"), a as f32 / 255.0)
}

fn path_data(path: &Path) -> String {
    let mut d = String::new();
    for subpath in &path.subpaths {
        if subpath.knots.is_empty() {
            continue;
        }
        let first = &subpath.knots[0];
        let _ = write!(d, "M{:.4},{:.4}", first.anchor.x, first.anchor.y);
        let segs = subpath.segments();
        for [_, c1, c2, end] in &segs {
            let _ = write!(
                d,
                " C{:.4},{:.4} {:.4},{:.4} {:.4},{:.4}",
                c1.x, c1.y, c2.x, c2.y, end.x, end.y
            );
        }
        if subpath.closed {
            d.push('Z');
        }
    }
    d
}

fn blend_mode_to_css(mode: BlendMode) -> Option<&'static str> {
    match mode {
        BlendMode::Normal | BlendMode::PassThrough => None,
        BlendMode::Multiply => Some("multiply"),
        BlendMode::Screen => Some("screen"),
        BlendMode::Overlay => Some("overlay"),
        BlendMode::Darken => Some("darken"),
        BlendMode::Lighten => Some("lighten"),
        BlendMode::ColorDodge => Some("color-dodge"),
        BlendMode::ColorBurn => Some("color-burn"),
        BlendMode::HardLight => Some("hard-light"),
        BlendMode::SoftLight => Some("soft-light"),
        BlendMode::Difference => Some("difference"),
        BlendMode::Exclusion => Some("exclusion"),
        BlendMode::Hue => Some("hue"),
        BlendMode::Saturation => Some("saturation"),
        BlendMode::Color => Some("color"),
        BlendMode::Luminosity => Some("luminosity"),
        _ => None,
    }
}

fn svg_stroke_attrs(stroke: &ShapeStroke) -> String {
    let mut s = format!(" stroke-width=\"{:.4}\"", stroke.width);
    match &stroke.paint {
        Fill::Solid(color) => {
            let (rgb, alpha) = svg_color(color);
            s.push_str(&format!(" stroke=\"{rgb}\""));
            let total_alpha = alpha * stroke.opacity;
            if total_alpha < 0.999 {
                s.push_str(&format!(" stroke-opacity=\"{total_alpha:.4}\""));
            }
        }
        _ => {
            s.push_str(" stroke=\"rgb(0,0,0)\"");
        }
    }
    match stroke.cap {
        LineCap::Butt => s.push_str(" stroke-linecap=\"butt\""),
        LineCap::Round => s.push_str(" stroke-linecap=\"round\""),
        LineCap::Square => s.push_str(" stroke-linecap=\"square\""),
    }
    match stroke.join {
        LineJoin::Miter => {
            s.push_str(" stroke-linejoin=\"miter\"");
            if stroke.miter_limit.is_finite() && stroke.miter_limit > 0.0 {
                s.push_str(&format!(" stroke-miterlimit=\"{:.4}\"", stroke.miter_limit));
            }
        }
        LineJoin::Round => s.push_str(" stroke-linejoin=\"round\""),
        LineJoin::Bevel => s.push_str(" stroke-linejoin=\"bevel\""),
    }
    if !stroke.dashes.is_empty() {
        let dashes: Vec<String> = stroke
            .dashes
            .iter()
            .map(|d| format!("{:.4}", d * stroke.width))
            .collect();
        s.push_str(&format!(" stroke-dasharray=\"{}\"", dashes.join(" ")));
        if stroke.dash_offset.abs() > 1e-4 {
            s.push_str(&format!(" stroke-dashoffset=\"{:.4}\"", stroke.dash_offset * stroke.width));
        }
    }
    s
}

fn svg_fill_attr(
    fill: &Option<Fill>,
    doc_bounds: photocraft_geom::Rect,
    defs: &mut String,
    id_counter: &mut u32,
    warnings: &mut Vec<String>,
) -> String {
    let Some(fill) = fill else {
        return " fill=\"none\"".to_string();
    };
    match fill {
        Fill::Solid(color) => {
            let (rgb, alpha) = svg_color(color);
            let mut s = format!(" fill=\"{rgb}\"");
            if alpha < 0.999 {
                s.push_str(&format!(" fill-opacity=\"{alpha:.4}\""));
            }
            s
        }
        Fill::Gradient { stops, angle, scale, style, reverse, offset, .. } => {
            *id_counter = id_counter.saturating_add(1);
            let grad_id = format!("grad{}", id_counter);
            let (from, to) = photocraft_compose::gradient_fill::handles(*style, *angle, *scale, *offset, doc_bounds);

            let mut sorted_stops = stops.clone();
            sorted_stops.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            if *reverse {
                sorted_stops.reverse();
            }

            match style {
                GradientStyle::Linear => {
                    let _ = write!(
                        defs,
                        "<linearGradient id=\"{grad_id}\" x1=\"{:.4}\" y1=\"{:.4}\" x2=\"{:.4}\" y2=\"{:.4}\" gradientUnits=\"userSpaceOnUse\">",
                        from[0], from[1], to[0], to[1]
                    );
                }
                GradientStyle::Radial => {
                    let dx = f64::from(to[0] - from[0]);
                    let dy = f64::from(to[1] - from[1]);
                    let r = dx.hypot(dy).max(0.001);
                    let _ = write!(
                        defs,
                        "<radialGradient id=\"{grad_id}\" cx=\"{:.4}\" cy=\"{:.4}\" r=\"{:.4}\" gradientUnits=\"userSpaceOnUse\">",
                        from[0], from[1], r
                    );
                }
                _ => {
                    warnings.push(format!("Gradient style {style:?} not natively supported in SVG; approximated as linear"));
                    let _ = write!(
                        defs,
                        "<linearGradient id=\"{grad_id}\" x1=\"{:.4}\" y1=\"{:.4}\" x2=\"{:.4}\" y2=\"{:.4}\" gradientUnits=\"userSpaceOnUse\">",
                        from[0], from[1], to[0], to[1]
                    );
                }
            }

            for (pos, col) in &sorted_stops {
                let actual_pos = if *reverse { 1.0 - pos } else { *pos };
                let (rgb, alpha) = svg_color(col);
                let _ = write!(
                    defs,
                    "<stop offset=\"{:.2}%\" stop-color=\"{rgb}\"",
                    actual_pos.clamp(0.0, 1.0) * 100.0
                );
                if alpha < 0.999 {
                    let _ = write!(defs, " stop-opacity=\"{alpha:.4}\"");
                }
                defs.push_str("/>");
            }

            if *style == GradientStyle::Radial {
                defs.push_str("</radialGradient>");
            } else {
                defs.push_str("</linearGradient>");
            }

            format!(" fill=\"url(#{grad_id})\"")
        }
        Fill::Pattern { name, .. } => {
            warnings.push(format!("Pattern fill '{name}' not directly exportable as vector; fill omitted"));
            " fill=\"none\"".to_string()
        }
    }
}

fn rasterize_layer_to_image_element(
    layer: &Layer,
    doc: &Document,
    w: &mut String,
    _warnings: &mut Vec<String>,
) -> Result<(), IoError> {
    let bounds = doc.bounds();
    let buf = photocraft_compose::render_layer(layer, bounds);
    let rgba = buf.to_rgba8();
    if rgba.pixels.iter().all(|&p| p == 0) {
        return Ok(());
    }
    let img = photocraft_codecs::Image::from_u8(
        rgba.width,
        rgba.height,
        photocraft_codecs::ChannelLayout::Rgba,
        rgba.pixels,
    )
    .map_err(|e| IoError::Svg(format!("failed to create image from layer '{}': {e}", layer.name)))?;
    let png_bytes = photocraft_codecs::encode(
        &img,
        photocraft_codecs::Format::Png,
        &photocraft_codecs::EncodeOptions::default(),
    )
    .map_err(|e| IoError::Svg(format!("failed to encode layer '{}' to PNG: {e}", layer.name)))?;
    let b64 = photocraft_paint::tile::b64_encode(&png_bytes);
    let _ = write!(
        w,
        "<image x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" href=\"data:image/png;base64,{b64}\"",
        bounds.width(),
        bounds.height()
    );
    if layer.opacity < 0.999 {
        let _ = write!(w, " opacity=\"{:.4}\"", layer.opacity);
    }
    if let Some(css_blend) = blend_mode_to_css(layer.blend) {
        let _ = write!(w, " style=\"mix-blend-mode:{css_blend}\"");
    }
    w.push_str("/>\n");
    Ok(())
}

fn write_layer(
    w: &mut String,
    layer: &Layer,
    doc: &Document,
    defs: &mut String,
    id_counter: &mut u32,
    depth: usize,
    warnings: &mut Vec<String>,
) -> Result<(), IoError> {
    if !layer.visible {
        return Ok(());
    }
    if depth > MAX_GROUP_DEPTH {
        warnings.push(format!("Layer nesting exceeded limit of {MAX_GROUP_DEPTH}; skipping deeper layers"));
        return Ok(());
    }

    let mut clip_attr = String::new();
    if let Some(vm) = &layer.vector_mask
        && vm.enabled
        && !vm.path.is_empty()
    {
        *id_counter = id_counter.saturating_add(1);
        let clip_id = format!("clip{}", id_counter);
        let d = path_data(&vm.path);
        let rule = if vm.path.fill_rule == FillRule::EvenOdd { " fill-rule=\"evenodd\"" } else { "" };
        let _ = write!(defs, "<clipPath id=\"{clip_id}\"><path d=\"{d}\"{rule}/></clipPath>");
        clip_attr = format!(" clip-path=\"url(#{clip_id})\"");
    }

    match &layer.content {
        LayerContent::Shape(shape) => {
            let d = path_data(&shape.path);
            let fill_attr = svg_fill_attr(&shape.fill, doc.bounds(), defs, id_counter, warnings);
            let stroke_attr = shape.stroke.as_ref().map(svg_stroke_attrs).unwrap_or_default();
            let rule = if shape.path.fill_rule == FillRule::EvenOdd { " fill-rule=\"evenodd\"" } else { "" };

            let mut extra = String::new();
            if layer.opacity < 0.999 {
                let _ = write!(extra, " opacity=\"{:.4}\"", layer.opacity);
            }
            if let Some(css_blend) = blend_mode_to_css(layer.blend) {
                let _ = write!(extra, " style=\"mix-blend-mode:{css_blend}\"");
            }
            extra.push_str(&clip_attr);

            let name_attr = if !layer.name.is_empty() { format!(" id=\"{}\"", escape_xml(&layer.name)) } else { String::new() };

            let _ = writeln!(w, "<path{name_attr} d=\"{d}\"{fill_attr}{stroke_attr}{rule}{extra}/>");
        }
        LayerContent::Text(text_layer) => {
            let (rgb, alpha) = svg_color(&text_layer.color);
            let mut style = format!("fill:{rgb};");
            if alpha < 0.999 {
                let _ = write!(style, "fill-opacity:{alpha:.4};");
            }
            if !text_layer.font_family.is_empty() {
                let _ = write!(style, "font-family:'{}';", escape_xml(&text_layer.font_family));
            }
            let _ = write!(style, "font-size:{:.2}pt;", text_layer.size_pt);

            let mut transform_attr = String::new();
            if text_layer.transform != Affine::IDENTITY {
                let [a, b, c, d, e, f] = text_layer.transform.m;
                transform_attr = format!(" transform=\"matrix({a:.4},{b:.4},{c:.4},{d:.4},{e:.4},{f:.4})\"");
            }

            let mut extra = String::new();
            if layer.opacity < 0.999 {
                let _ = write!(extra, " opacity=\"{:.4}\"", layer.opacity);
            }
            if let Some(css_blend) = blend_mode_to_css(layer.blend) {
                let _ = write!(extra, " style=\"mix-blend-mode:{css_blend}\"");
            }
            extra.push_str(&clip_attr);

            let name_attr = if !layer.name.is_empty() { format!(" id=\"{}\"", escape_xml(&layer.name)) } else { String::new() };

            let escaped_text = escape_xml(&text_layer.text);
            let _ = writeln!(w, "<text{name_attr} style=\"{style}\"{transform_attr}{extra}>{escaped_text}</text>");
        }
        LayerContent::Group(group) => {
            let mut g_tag = String::from("<g");
            if !layer.name.is_empty() {
                let _ = write!(g_tag, " id=\"{}\"", escape_xml(&layer.name));
            }
            if layer.opacity < 0.999 {
                let _ = write!(g_tag, " opacity=\"{:.4}\"", layer.opacity);
            }
            if let Some(css_blend) = blend_mode_to_css(layer.blend) {
                let _ = write!(g_tag, " style=\"mix-blend-mode:{css_blend}\"");
            }
            g_tag.push_str(&clip_attr);
            g_tag.push_str(">\n");
            w.push_str(&g_tag);

            for child in &group.children {
                write_layer(w, child, doc, defs, id_counter, depth + 1, warnings)?;
            }

            w.push_str("</g>\n");
        }
        LayerContent::Raster(_) | LayerContent::Smart(_) => {
            rasterize_layer_to_image_element(layer, doc, w, warnings)?;
        }
        LayerContent::Adjustment(_) | LayerContent::Fill(_) => {
            warnings.push(format!("Adjustment/Fill layer '{}' flattened into composite", layer.name));
            rasterize_layer_to_image_element(layer, doc, w, warnings)?;
        }
    }

    Ok(())
}

/// Converts a Document to an SVG string and returns the UTF-8 bytes and any conversion warnings.
pub fn export(doc: &Document) -> Result<ExportResult, IoError> {
    let mut warnings = Vec::new();
    let width = doc.size.width;
    let height = doc.size.height;

    let mut body = String::new();
    let mut defs = String::new();
    let mut id_counter = 0u32;

    for layer in &doc.layers {
        write_layer(&mut body, layer, doc, &mut defs, &mut id_counter, 0, &mut warnings)?;
    }

    let mut svg = String::new();
    svg.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        svg,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" viewBox=\"0 0 {width} {height}\" width=\"{width}\" height=\"{height}\">"
    );

    if !defs.is_empty() {
        svg.push_str("<defs>\n");
        svg.push_str(&defs);
        svg.push_str("\n</defs>\n");
    }

    svg.push_str(&body);
    svg.push_str("</svg>\n");

    Ok(ExportResult { bytes: svg.into_bytes(), warnings })
}

/// Converts a Document to a gzip-compressed SVG (.svgz).
pub fn export_svgz(doc: &Document) -> Result<ExportResult, IoError> {
    let res = export(doc)?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&res.bytes).map_err(|e| IoError::Svg(format!("failed to compress svgz: {e}")))?;
    let compressed = encoder.finish().map_err(|e| IoError::Svg(format!("failed to finish svgz: {e}")))?;
    Ok(ExportResult { bytes: compressed, warnings: res.warnings })
}
#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::vector::{ShapeLayer, ShapeStroke, Subpath};
    use photocraft_doc::Size;
    use std::io::Read as _;

    #[test]
    fn empty_document() {
        let doc = Document::new("empty.svg", Size::new(100, 100), ColorMode::Rgb, SampleType::U8);
        let res = export(&doc).unwrap();
        let s = String::from_utf8(res.bytes).unwrap();
        assert!(s.contains("<svg "));
        assert!(s.contains("viewBox=\"0 0 100 100\""));
        assert!(s.contains("</svg>"));
    }

    #[test]
    fn shape_layer_roundtrip() {
        let mut doc = Document::new("shape.svg", Size::new(200, 150), ColorMode::Rgb, SampleType::U8);
        let path = Path::new(vec![Subpath::polygon(&[(10.0, 10.0), (50.0, 10.0), (50.0, 40.0), (10.0, 40.0)])]);
        let shape = ShapeLayer {
            path,
            fill: Some(Fill::Solid(Color::rgba(1.0, 0.0, 0.0, 1.0))),
            stroke: Some(ShapeStroke {
                width: 2.0,
                paint: Fill::Solid(Color::rgba(0.0, 0.0, 0.0, 1.0)),
                ..Default::default()
            }),
            ..Default::default()
        };
        doc.layers.push(Layer::new("Box", LayerContent::Shape(shape)));

        let res = export(&doc).unwrap();
        let s = String::from_utf8(res.bytes.clone()).unwrap();
        assert!(s.contains("id=\"Box\""));
        assert!(s.contains("fill=\"rgb(255,0,0)\""));
        assert!(s.contains("stroke-width=\"2.0000\""));
        assert!(s.contains("stroke=\"rgb(0,0,0)\""));

        // Roundtrip via svg::import
        let imp = crate::svg::import_svg("shape.svg", &res.bytes).unwrap();
        assert_eq!((imp.document.size.width, imp.document.size.height), (200, 150));
        assert!(!imp.document.layers.is_empty());
    }

    #[test]
    fn text_layer_export() {
        let mut doc = Document::new("text.svg", Size::new(200, 100), ColorMode::Rgb, SampleType::U8);
        let text = photocraft_doc::TextLayer {
            text: "Hello PhotoCraft".into(),
            font_family: "Arial".into(),
            size_pt: 18.0,
            color: Color::rgba(0.0, 0.0, 1.0, 1.0),
            ..Default::default()
        };
        doc.layers.push(Layer::new("Greeting", LayerContent::Text(text)));
        let res = export(&doc).unwrap();
        let s = String::from_utf8(res.bytes).unwrap();
        assert!(s.contains("Hello PhotoCraft"));
        assert!(s.contains("font-family:'Arial'"));
        assert!(s.contains("font-size:18.00pt"));
        assert!(s.contains("fill:rgb(0,0,255)"));
    }

    #[test]
    fn raster_layer_export() {
        let mut doc = Document::new("raster.svg", Size::new(4, 4), ColorMode::Rgb, SampleType::U8);
        let surf = photocraft_raster::Surface::with_default(doc.pixel_format(), &[1.0, 0.0, 0.0, 1.0]);
        doc.layers.push(Layer::new("Red", LayerContent::Raster(surf)));

        let res = export(&doc).unwrap();
        let s = String::from_utf8(res.bytes).unwrap();
        assert!(s.contains("<image "));
        assert!(s.contains("href=\"data:image/png;base64,"));
    }

    #[test]
    fn svgz_compression_decompression() {
        let doc = Document::new("test.svgz", Size::new(50, 50), ColorMode::Rgb, SampleType::U8);
        let uncompressed = export(&doc).unwrap();
        let compressed = export_svgz(&doc).unwrap();
        assert_ne!(uncompressed.bytes, compressed.bytes);

        let mut decoder = flate2::read::GzDecoder::new(&compressed.bytes[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert_eq!(decompressed, uncompressed.bytes);
    }
}
