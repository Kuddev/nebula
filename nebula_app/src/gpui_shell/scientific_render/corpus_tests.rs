//! GPUI adapter checks complement the renderer-independent document corpus.
use super::*;
use crate::scientific_corpus::{DOCUMENTS, cases, read_document};

#[test]
fn corpus_molecular_svg_is_rasterized_by_the_actual_gpui_adapter() {
    let renderer = SvgRenderer::new(Arc::new(()));
    let mut cache = crate::render_cache::RenderCache::new(4 * 1024 * 1024, 64);
    let mut images = Vec::new();
    for (name, _, _) in DOCUMENTS {
        for case in
            cases(&read_document(name)).into_iter().filter(|case| case.molecule && !case.fallback)
        {
            let svg = crate::chemistry::render_smiles(&case.source, false).unwrap();
            let image = renderer
                .render_single_frame(svg.as_bytes(), 1.0)
                .unwrap_or_else(|error| panic!("{name}:{}: {error}", case.line));
            let bytes = image.as_bytes(0).unwrap();
            let size = image.size(0);
            assert_eq!(
                bytes.len(),
                u32::from(size.width) as usize * u32::from(size.height) as usize * 4
            );
            assert!(
                bytes.chunks_exact(4).any(|pixel| pixel[3] != 0),
                "{name}:{} is blank",
                case.line
            );
            images.push(Arc::downgrade(&image));
            let resource = Resource::Molecule(image);
            let charge = resource.bytes() + 256;
            cache.insert((name, case.line), resource, charge);
            let retained_bytes: usize = images
                .iter()
                .filter_map(|image| image.upgrade())
                .map(|image| image.as_bytes(0).unwrap().len())
                .sum();
            assert!(
                retained_bytes <= 4 * 1024 * 1024,
                "physical image bytes exceeded the shared budget"
            );
        }
    }
}

#[test]
fn mathematical_gpui_adapter_keeps_the_shared_pixel_buffer_and_geometry() {
    let rasterizer = crate::math::rasterizer::MathGlyphRasterizer::new().unwrap();
    for case in cases(&read_document("scientific-rendering-acceptance-paper.md"))
        .into_iter()
        .filter(|case| !case.molecule && !case.fallback)
        .take(12)
    {
        let layout =
            compile_formula_source(&case.source, case.display, 18.0, 1.0, DEFAULT_LIMITS).unwrap();
        let expected = crate::math::bitmap::compose(&rasterizer, &layout, 1.25, [0, 0, 0]).unwrap();
        let (image, geometry) =
            compose_image(&rasterizer, &layout, 1.25, Rgba { r: 0.0, g: 0.0, b: 0.0, a: 1.0 })
                .unwrap();
        assert_eq!(image.as_bytes(0).unwrap(), expected.pixels);
        assert_eq!(geometry.baseline, expected.baseline);
        assert_eq!(geometry.width, expected.width);
        assert_eq!(geometry.height, expected.height);
    }
}

#[test]
#[ignore = "bounded native SVG raster benchmark; no window or real AI processes"]
fn measure_molecular_raster_stages() {
    let renderer = SvgRenderer::new(Arc::new(()));
    let warm = crate::chemistry::render_smiles("CCO", false).unwrap();
    drop(renderer.render_single_frame(warm.as_bytes(), 1.0).unwrap());
    for (name, _, _) in DOCUMENTS {
        let mut samples = Vec::new();
        for case in
            cases(&read_document(name)).into_iter().filter(|case| case.molecule && !case.fallback)
        {
            let svg = crate::chemistry::render_smiles(&case.source, false).unwrap();
            let start = std::time::Instant::now();
            std::hint::black_box(renderer.render_single_frame(svg.as_bytes(), 1.0).unwrap());
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        if !samples.is_empty() {
            samples.sort_by(f64::total_cmp);
            let p = |n: usize| samples[(samples.len() * n).div_ceil(100) - 1];
            crate::gpui_shell::try_write_stderr(format_args!(
                "{name}:gpui_svg_raster,samples={},median_ms={:.4},p95_ms={:.4},p99_ms={:.4}",
                samples.len(),
                p(50),
                p(95),
                p(99)
            ));
        }
    }
}
