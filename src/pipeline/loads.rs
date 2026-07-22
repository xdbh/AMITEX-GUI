/// Voigt-notation strain components, in the order the legacy `load_0N.xml` files use them.
pub(crate) const VOIGT_COMPONENTS: [&str; 6] = ["xx", "yy", "zz", "xy", "xz", "yz"];

/// Case directory/label name for Voigt component `index` (0 = xx, .. 5 = yz), e.g. `load_xy`.
/// Named after the strain direction rather than the legacy `load_0N` numbering, since the
/// direction is what the case actually means.
pub(crate) fn case_label(index: usize) -> String {
    format!("load_{}", VOIGT_COMPONENTS[index])
}

/// Generates the `char.xml` loading file for load case `unit_component` (0 = xx, .. 5 = yz):
/// a unit strain in that Voigt direction, all others held at zero. The 6 legacy
/// `load_0N.xml` files in `20260716142000/` are byte-identical except for exactly this
/// value, so this replaces importing one file per case.
pub(crate) fn generate_load_xml(unit_component: usize) -> String {
    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<Loading_Output>\n\n");
    xml.push_str("<Output>\n    <vtk_StressStrain Strain=\"1\" Stress=\"1\"/>\n</Output>\n\n");
    xml.push_str("<Loading Tag=\"1\">\n");
    xml.push_str("    <Time_Discretization Discretization=\"Linear\" Nincr=\"1\" Tfinal=\"1.\"/>\n");
    xml.push_str("    <Output_vtkList>\n      1\n    </Output_vtkList>\n");
    for (i, component) in VOIGT_COMPONENTS.iter().enumerate() {
        let value = if i == unit_component { "1." } else { "0." };
        xml.push_str(&format!(
            "    <{component} Driving=\"Strain\" Evolution=\"Linear\" Value=\"{value}\"/>\n"
        ));
    }
    xml.push_str("</Loading>\n\n</Loading_Output>\n");
    xml
}
