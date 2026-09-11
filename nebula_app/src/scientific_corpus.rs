//! The maintained documents are the source of regression inputs and benchmarks.

use crate::math::layout::MathLayout;
use crate::math::{DEFAULT_LIMITS, compile_formula_source};
use crate::render_cache::RenderCache;
use markdown::mdast::Node;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

pub(crate) const DOCUMENTS: [(&str, usize, usize); 4] = [
    ("math-rendering-test.md", 50, 0),
    ("chemistry-rendering-test.md", 40, 55),
    ("biology-rendering-test.md", 64, 64),
    ("scientific-rendering-acceptance-paper.md", 35, 10),
];

#[derive(Debug)]
pub(crate) struct Case {
    pub(crate) source: String,
    pub(crate) molecule: bool,
    pub(crate) display: bool,
    pub(crate) fallback: bool,
    pub(crate) preserve: bool,
    pub(crate) line: usize,
}

fn document_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs").join(name)
}

pub(crate) fn read_document(name: &str) -> String {
    let path = document_path(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert!(text.len() <= 128 * 1024, "default regression corpus must stay small");
    text
}

pub(crate) fn cases(source: &str) -> Vec<Case> {
    let mut options = markdown::ParseOptions::gfm();
    options.constructs.math_flow = true;
    options.constructs.math_text = true;
    let tree = markdown::to_mdast(source, &options).expect("valid Markdown corpus");
    let mut output = Vec::new();
    let mut expectation = (false, false);
    collect(&tree, &mut expectation, &mut output);
    assert_eq!(expectation, (false, false), "orphan corpus expectation");
    output
}

fn collect(node: &Node, expectation: &mut (bool, bool), output: &mut Vec<Case>) {
    let content = match node {
        Node::Html(html) if html.value.trim() == "<!-- pebrel-test: source-fallback -->" => {
            assert_eq!(*expectation, (false, false), "unconsumed expectation marker");
            *expectation = (true, false);
            return;
        },
        Node::Html(html) if html.value.trim() == "<!-- pebrel-test: preserve-source -->" => {
            assert_eq!(*expectation, (false, false), "unconsumed expectation marker");
            *expectation = (false, true);
            return;
        },
        Node::Math(math) => Some((&math.value, false, true)),
        Node::InlineMath(math) => Some((&math.value, false, false)),
        Node::Code(code)
            if code.lang.as_deref().is_some_and(|lang| lang.eq_ignore_ascii_case("smiles")) =>
        {
            Some((&code.value, true, true))
        },
        _ => None,
    };
    if let Some((source, molecule, display)) = content {
        output.push(Case {
            source: source.clone(),
            molecule,
            display,
            fallback: expectation.0,
            preserve: expectation.1,
            line: node.position().map_or(0, |position| position.start.line),
        });
        *expectation = (false, false);
    } else if let Some(children) = node.children() {
        for child in children {
            collect(child, expectation, output);
        }
    }
}

fn assert_math_pixels(layout: &MathLayout, scale: f32) {
    assert!(layout.metrics.width.is_finite() && layout.metrics.width >= 0.0);
    assert!(layout.metrics.height.is_finite() && layout.metrics.depth.is_finite());
    let rasterizer = crate::math::rasterizer::MathGlyphRasterizer::new().unwrap();
    let geometry =
        crate::math::bitmap::compose(&rasterizer, layout, scale, [0, 0, 0]).expect("formula image");
    let bytes = &geometry.pixels;
    assert_eq!(bytes.len(), geometry.width as usize * geometry.height as usize * 4);
    assert!(geometry.width > 0 && geometry.height > 0);
    if !layout.glyphs.is_empty() || !layout.rules.is_empty() {
        assert!(bytes.chunks_exact(4).any(|pixel| pixel[3] != 0), "empty mathematical image");
    }
}

#[test]
fn maintained_scientific_documents_generate_math_pixels_and_molecular_svg() {
    let mut failures = Vec::new();
    for (name, minimum_math, minimum_molecules) in DOCUMENTS {
        let input = read_document(name);
        let cases = cases(&input);
        assert!(
            cases.iter().filter(|case| !case.molecule && !case.fallback).count() >= minimum_math,
            "{name}: mathematical coverage shrank"
        );
        assert!(
            cases.iter().filter(|case| case.molecule && !case.fallback).count()
                >= minimum_molecules,
            "{name}: molecular coverage shrank"
        );
        for case in cases {
            if case.molecule {
                let result = crate::chemistry::render_smiles(&case.source, false);
                match (case.fallback, result) {
                    (true, Err(_)) => {},
                    (false, Ok(svg)) => {
                        assert!(svg.starts_with("<svg"));
                        assert!(svg.contains("viewBox="));
                        assert!(!svg.contains("NaN") && !svg.contains("Infinity"));
                    },
                    (expected, actual) => failures.push(format!(
                        "{name}:{} expected fallback={expected}; result={:?}; source={}",
                        case.line,
                        actual.map(|_| "rendered"),
                        case.source
                    )),
                }
            } else {
                let result =
                    compile_formula_source(&case.source, case.display, 18.0, 1.0, DEFAULT_LIMITS);
                match (case.fallback, result) {
                    (true, Err(_)) => {},
                    (false, Ok(layout)) => {
                        assert_math_pixels(&layout, 1.0);
                        if case.preserve {
                            let font = crate::math::font::MathFont::load().unwrap();
                            if !case.source.contains('=') {
                                let equal = font.glyph_id('=').unwrap().0;
                                assert!(
                                    !layout.glyphs.iter().any(|op| op.glyph_id == equal),
                                    "must not invent a missing equality"
                                );
                            }
                            if case.source.contains(",dx") {
                                let comma = font.glyph_id(',').unwrap().0;
                                assert!(
                                    layout.glyphs.iter().any(|op| op.glyph_id == comma),
                                    "comma must not become invisible spacing"
                                );
                            }
                        }
                    },
                    (expected, actual) => failures.push(format!(
                        "{name}:{} expected fallback={expected}; result={:?}; source={}",
                        case.line,
                        actual.map(|_| "rendered"),
                        case.source
                    )),
                }
            }
        }
    }
    assert!(failures.is_empty(), "Scientific corpus failures:\n{}", failures.join("\n"));
}

#[test]
fn repeated_corpus_resize_uses_bounded_lru_and_keeps_geometry_finite() {
    let cases = cases(&read_document("scientific-rendering-acceptance-paper.md"));
    let cases: Vec<_> =
        cases.iter().filter(|case| !case.molecule && !case.fallback).take(12).collect();
    let mut cache = RenderCache::new(256 * 1024, 32);
    for width in [880.0_f32, 320.0, 640.0, 240.0, 880.0, 320.0] {
        for (index, case) in cases.iter().enumerate() {
            let layout =
                compile_formula_source(&case.source, case.display, 18.0, 1.0, DEFAULT_LIMITS)
                    .unwrap();
            let pixel_size =
                (18.0 * (width / layout.metrics.width.max(1.0)).min(1.0) * 0.98).max(6.0);
            let key = (index, pixel_size.to_bits());
            if cache.get(&key).is_none() {
                let layout = compile_formula_source(
                    &case.source,
                    case.display,
                    pixel_size,
                    1.0,
                    DEFAULT_LIMITS,
                )
                .unwrap();
                assert_math_pixels(&layout, 1.25);
                let bytes = layout.allocated_bytes() + 256;
                cache.insert(key, Arc::new(layout), bytes);
            }
            assert!(cache.used_bytes() <= 256 * 1024);
            assert!(cache.len() <= 32);
        }
    }
}

fn print_distribution(label: &str, values: &mut [f64]) {
    if values.is_empty() {
        return;
    }
    values.sort_by(f64::total_cmp);
    let percentile = |n: usize| values[(values.len() * n).div_ceil(100).saturating_sub(1)];
    println!(
        "{label},samples={},median_ms={:.4},p95_ms={:.4},p99_ms={:.4}",
        values.len(),
        percentile(50),
        percentile(95),
        percentile(99)
    );
}

#[test]
#[ignore = "bounded informational corpus benchmark; run separately with one test thread"]
fn measure_scientific_document_stages() {
    println!(
        "Scientific document CPU stages; no real AI sessions, no 100 MB allocation, no FPS claim."
    );
    let rasterizer = crate::math::rasterizer::MathGlyphRasterizer::new().unwrap();
    for (name, _, _) in DOCUMENTS {
        let source = read_document(name);
        let mut parsing = Vec::new();
        for _ in 0..10 {
            let start = Instant::now();
            std::hint::black_box(cases(&source));
            parsing.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        print_distribution(&format!("{name}:markdown_parse"), &mut parsing);
        let mut compile = Vec::new();
        let mut raster = Vec::new();
        let mut molecules = Vec::new();
        for case in cases(&source).into_iter().filter(|case| !case.fallback) {
            if case.molecule {
                let start = Instant::now();
                let svg = crate::chemistry::render_smiles(&case.source, false).unwrap();
                std::hint::black_box(svg);
                molecules.push(start.elapsed().as_secs_f64() * 1000.0);
            } else {
                let start = Instant::now();
                let layout =
                    compile_formula_source(&case.source, case.display, 18.0, 1.0, DEFAULT_LIMITS)
                        .unwrap();
                compile.push(start.elapsed().as_secs_f64() * 1000.0);
                let start = Instant::now();
                std::hint::black_box(
                    crate::math::bitmap::compose(&rasterizer, &layout, 1.0, [0, 0, 0]).unwrap(),
                );
                raster.push(start.elapsed().as_secs_f64() * 1000.0);
            }
        }
        print_distribution(&format!("{name}:math_compile"), &mut compile);
        print_distribution(&format!("{name}:math_bitmap"), &mut raster);
        print_distribution(&format!("{name}:molecule_svg"), &mut molecules);
    }
}
