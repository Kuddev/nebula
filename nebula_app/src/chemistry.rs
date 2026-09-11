//! Shared bounded SMILES parsing and 2D depiction for documents and terminals.

use chematic_depict::{RenderOptions, compute_layout, render_svg_opts};
use chematic_smiles::{SmilesParseLimits, parse_with_limits};

/// Temporarily disabled while the product's memory budget is investigated.
/// Keep depiction code available; UI adapters must return before scheduling work.
pub(crate) const STRUCTURE_RENDERING_ENABLED: bool = false;

pub(crate) const MAX_SOURCE_BYTES: usize = 2048;
pub(crate) const IMAGE_WIDTH: u32 = 480;
pub(crate) const IMAGE_HEIGHT: u32 = 240;

/// Called by background adapters. Invalid input remains source text in the UI.
pub(crate) fn render_smiles(source: &str, dark: bool) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty() || source.len() > MAX_SOURCE_BYTES || source.contains(['\n', '\r']) {
        return Err("Expected one SMILES molecule (up to 2048 bytes)".into());
    }
    // This backend does not yet project atom parity or alkene direction into
    // correct 2D stereobonds. Preserve those expressions as source.
    if source.contains(['@', '/', '\\']) {
        return Err(
            "Stereochemical SMILES is preserved as source until depiction is supported".into()
        );
    }
    let limits =
        SmilesParseLimits { max_input_bytes: MAX_SOURCE_BYTES, max_atoms: 128, max_bonds: 192 };
    let molecule = parse_with_limits(source, &limits).map_err(|error| error.to_string())?;
    if molecule.atom_count() == 0 {
        return Err("The molecule has no atoms".into());
    }
    let layout = compute_layout(&molecule);
    let options = RenderOptions {
        width: Some(IMAGE_WIDTH),
        height: Some(IMAGE_HEIGHT),
        dark,
        background: "transparent".into(),
        padding: 12.0,
        ..Default::default()
    };
    let svg = render_svg_opts(&molecule, &layout, &options);
    if svg.len() > 512 * 1024 || svg.contains("NaN") || svg.contains("Infinity") {
        return Err("The molecule could not be laid out within the preview budget".into());
    }
    Ok(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depicts_rings_branches_aromatics_and_charges() {
        for source in ["c1ccccc1", "CC(=O)Oc1ccccc1C(=O)O", "[NH4+].[Cl-]"] {
            for dark in [false, true] {
                let svg = render_smiles(source, dark).unwrap();
                assert!(svg.starts_with("<svg"));
                assert!(svg.contains("viewBox="));
                assert!(!svg.contains("NaN"));
            }
        }
    }

    #[test]
    fn stereo_source_is_preserved_when_the_backend_cannot_depict_it() {
        for source in ["C[C@H](O)C(=O)O", "C[C@@H](O)C(=O)O", "F/C=C/F", "F/C=C\\F"] {
            assert!(render_smiles(source, false).is_err());
        }
    }

    #[test]
    fn invalid_and_oversized_input_is_not_drawn_as_a_different_molecule() {
        for source in
            ["", "C1CC", "not a molecule", "CCO\nc1ccccc1", &"C".repeat(129), &"C".repeat(2049)]
        {
            assert!(render_smiles(source, false).is_err(), "{source}");
        }
    }
}
