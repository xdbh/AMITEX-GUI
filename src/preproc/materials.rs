//! In-GUI material law editor and `mat.xml` generation, replacing the old
//! browse-to-a-file workflow. Coefficient specs below were read directly from AMITEX's
//! Fortran source (`libAmitex/src/materiaux/*.f90`) rather than the public docs, which only
//! document `elasiso`'s coefficients — the rest are documented only in that source, which
//! isn't publicly hosted. Zone-varying coefficient syntax (`Type="Constant_Zone"`), on the
//! other hand, *is* public — see https://amitexfftp.github.io/AMITEX/user_guide/materials.html.
//!
//! Only `ElasticIsotropic` and `ElasticOrthotropic` are wired up to generate real XML. Both
//! produce an instantaneous linear `STRESS = C:STRAIN` response, which is what this app's
//! elastic-homogenization pipeline actually needs (6 unit-strain cases -> read back stress ->
//! assemble a stiffness matrix, see `postproc::moduli`). The other 7 native laws are named
//! here (so they're visible as a roadmap) but deliberately not selectable yet:
//! - `ElasticIsotropicEigenstrain`/`ElasticIsotropicLargeStrain`/`ElasticOrthotropicLargeStrain`:
//!   same underlying elasticity, but a nonzero eigenstrain or finite-strain kinematics would
//!   silently invalidate `postproc::moduli`'s raw-stress-column extraction.
//! - `Thermoelastic`/`ThermoelasticExternalParam`: need a temperature/external-field loading
//!   value this app's `char.xml` generator (`pipeline::loads`) doesn't produce.
//! - `ImposedStress`: not a real constitutive response (outputs a fixed stress regardless of
//!   applied strain) — there's no modulus to homogenize.
//! - `ViscoelasticMaxwell`: needs genuine time-stepped loading history, not one static
//!   unit-strain case per direction.

use crate::postproc::vtkio::{distinct_sorted_values, VtkGrid};
use anyhow::{bail, Context};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LawKind {
    ElasticIsotropic,
    ElasticOrthotropic,
    ElasticIsotropicEigenstrain,
    ElasticIsotropicLargeStrain,
    ElasticOrthotropicLargeStrain,
    Thermoelastic,
    ThermoelasticExternalParam,
    ImposedStress,
    ViscoelasticMaxwell,
}

impl LawKind {
    pub(crate) const ALL: [LawKind; 9] = [
        LawKind::ElasticIsotropic,
        LawKind::ElasticOrthotropic,
        LawKind::ElasticIsotropicEigenstrain,
        LawKind::ElasticIsotropicLargeStrain,
        LawKind::ElasticOrthotropicLargeStrain,
        LawKind::Thermoelastic,
        LawKind::ThermoelasticExternalParam,
        LawKind::ImposedStress,
        LawKind::ViscoelasticMaxwell,
    ];

    /// Human-readable name shown in the GUI's law picker.
    pub(crate) fn label(&self) -> &'static str {
        match self {
            LawKind::ElasticIsotropic => "Elastic — isotropic",
            LawKind::ElasticOrthotropic => "Elastic — orthotropic",
            LawKind::ElasticIsotropicEigenstrain => "Elastic — isotropic + eigenstrain (not yet implemented)",
            LawKind::ElasticIsotropicLargeStrain => "Elastic — isotropic, large strain (not yet implemented)",
            LawKind::ElasticOrthotropicLargeStrain => "Elastic — orthotropic, large strain (not yet implemented)",
            LawKind::Thermoelastic => "Thermoelastic — isotropic (not yet implemented)",
            LawKind::ThermoelasticExternalParam => "Thermoelastic — external parameter (not yet implemented)",
            LawKind::ImposedStress => "Imposed stress (not yet implemented)",
            LawKind::ViscoelasticMaxwell => "Viscoelastic — generalized Maxwell (not yet implemented)",
        }
    }

    /// The AMITEX `Law="..."` attribute value (`libAmitex/src/materiaux/<name>.f90`).
    pub(crate) fn amitex_name(&self) -> &'static str {
        match self {
            LawKind::ElasticIsotropic => "elasiso",
            LawKind::ElasticOrthotropic => "elasaniso",
            LawKind::ElasticIsotropicEigenstrain => "elasiso_eigs",
            LawKind::ElasticIsotropicLargeStrain => "elasiso_GD",
            LawKind::ElasticOrthotropicLargeStrain => "elasaniso_GD",
            LawKind::Thermoelastic => "thermoelasiso",
            LawKind::ThermoelasticExternalParam => "paramextelasiso",
            LawKind::ImposedStress => "contrainte_imposee",
            LawKind::ViscoelasticMaxwell => "viscoelas_maxwell",
        }
    }

    pub(crate) fn implemented(&self) -> bool {
        matches!(self, LawKind::ElasticIsotropic | LawKind::ElasticOrthotropic)
    }
}

impl Default for LawKind {
    fn default() -> Self {
        LawKind::ElasticIsotropic
    }
}

/// A single scalar material coefficient: either one value for the whole material
/// (`Type="Constant"`), or one value per zone (`Type="Constant_Zone"`), in ascending `numZ`
/// order — see `detect_zone_ids`. AMITEX lets every `Coeff` choose independently, so each
/// field in `IsotropicCoeffs`/`OrthotropicCoeffs` carries its own `ZoneValue`.
#[derive(Clone)]
pub(crate) enum ZoneValue {
    Constant(f64),
    PerZone(Vec<f64>),
}

impl Default for ZoneValue {
    fn default() -> Self {
        ZoneValue::Constant(0.0)
    }
}

impl ZoneValue {
    pub(crate) fn is_per_zone(&self) -> bool {
        matches!(self, ZoneValue::PerZone(_))
    }

    /// Switches between `Constant`/`PerZone`, preserving whatever value(s) can carry over.
    pub(crate) fn set_per_zone(&mut self, per_zone: bool, num_zones: usize) {
        *self = match (per_zone, std::mem::take(self)) {
            (false, ZoneValue::Constant(v)) => ZoneValue::Constant(v),
            (false, ZoneValue::PerZone(vals)) => ZoneValue::Constant(vals.first().copied().unwrap_or(0.0)),
            (true, ZoneValue::Constant(v)) => ZoneValue::PerZone(vec![v; num_zones]),
            (true, ZoneValue::PerZone(vals)) => ZoneValue::PerZone(vals),
        };
        self.resize(num_zones);
    }

    /// Keeps a `PerZone` list's length equal to `num_zones` as the zone-ID VTK/material
    /// selection changes. New slots default to the last existing value (falling back to 0.0),
    /// so growing the zone count doesn't reset values already entered for zones that still
    /// exist. No-op for `Constant`, which has no per-zone length to track.
    pub(crate) fn resize(&mut self, num_zones: usize) {
        if let ZoneValue::PerZone(vals) = self {
            if vals.len() != num_zones {
                let fill = vals.last().copied().unwrap_or(0.0);
                vals.resize(num_zones, fill);
            }
        }
    }

    /// Broadcasts `Constant` to `num_zones` copies, or returns the per-zone list as-is after
    /// checking its length matches — lets a coefficient that mixes a `Constant` field with a
    /// `PerZone` one (e.g. isotropic `E` per-zone but `nu` constant) be walked zone-by-zone.
    fn resolve(&self, num_zones: usize, label: &str) -> anyhow::Result<Vec<f64>> {
        match self {
            ZoneValue::Constant(v) => Ok(vec![*v; num_zones]),
            ZoneValue::PerZone(vals) => {
                if vals.len() != num_zones {
                    bail!(
                        "{label}: {} per-zone value(s) entered, but this material has {num_zones} zone(s)",
                        vals.len()
                    );
                }
                Ok(vals.clone())
            }
        }
    }
}

/// Same as `ZoneValue`, but for a 3-component vector coefficient (`elasaniso`'s `e1`/`e2` local
/// basis vectors) — the whole vector toggles between constant and per-zone together, rather
/// than each axis independently, since a per-zone *orientation* is the actual use case (each
/// grain/zone in a polycrystal gets its own basis, not just one axis of it).
#[derive(Clone)]
pub(crate) enum ZoneVec3 {
    Constant([f64; 3]),
    PerZone(Vec<[f64; 3]>),
}

impl ZoneVec3 {
    pub(crate) fn is_per_zone(&self) -> bool {
        matches!(self, ZoneVec3::PerZone(_))
    }

    pub(crate) fn set_per_zone(&mut self, per_zone: bool, num_zones: usize) {
        *self = match (per_zone, std::mem::replace(self, ZoneVec3::Constant([0.0; 3]))) {
            (false, ZoneVec3::Constant(v)) => ZoneVec3::Constant(v),
            (false, ZoneVec3::PerZone(vals)) => ZoneVec3::Constant(vals.first().copied().unwrap_or([0.0; 3])),
            (true, ZoneVec3::Constant(v)) => ZoneVec3::PerZone(vec![v; num_zones]),
            (true, ZoneVec3::PerZone(vals)) => ZoneVec3::PerZone(vals),
        };
        self.resize(num_zones);
    }

    pub(crate) fn resize(&mut self, num_zones: usize) {
        if let ZoneVec3::PerZone(vals) = self {
            if vals.len() != num_zones {
                let fill = vals.last().copied().unwrap_or([0.0; 3]);
                vals.resize(num_zones, fill);
            }
        }
    }

    fn resolve(&self, num_zones: usize, label: &str) -> anyhow::Result<Vec<[f64; 3]>> {
        match self {
            ZoneVec3::Constant(v) => Ok(vec![*v; num_zones]),
            ZoneVec3::PerZone(vals) => {
                if vals.len() != num_zones {
                    bail!(
                        "{label}: {} per-zone value(s) entered, but this material has {num_zones} zone(s)",
                        vals.len()
                    );
                }
                Ok(vals.clone())
            }
        }
    }
}

/// `elasiso` (isotropic elasticity) coefficients, entered as engineering constants — matches
/// what `postproc::moduli` reports back after a run — and converted to the Lamé coefficients
/// `elasiso.f90` actually takes (`PROPS(1)=lambda, PROPS(2)=mu`) at XML-generation time.
#[derive(Clone, Default)]
pub(crate) struct IsotropicCoeffs {
    pub(crate) e: ZoneValue,
    pub(crate) nu: ZoneValue,
}

impl IsotropicCoeffs {
    /// Rejects inputs that make the Lamé conversion diverge or go unphysical: `nu=0.5` is the
    /// incompressible limit, where `lambda`'s `(1-2*nu)` denominator hits zero and `lambda`
    /// goes to infinity — `elasiso` (a finite-stiffness FFT scheme) can't represent that, and
    /// AMITEX silently turns it into `Critere d'equilibre NaN`/`Contrainte ... NaN (behavior)`
    /// partway through the run instead of rejecting it up front.
    fn validate_one(e: f64, nu: f64) -> anyhow::Result<()> {
        if e <= 0.0 {
            bail!("E must be positive (got {e})");
        }
        if !(-1.0 < nu && nu < 0.5) {
            bail!(
                "nu must be strictly between -1 and 0.5 (got {nu}) — nu=0.5 is the incompressible \
                 limit, where lambda diverges to infinity"
            );
        }
        Ok(())
    }

    /// Standard isotropic-elasticity conversion from engineering constants to Lamé
    /// coefficients: `lambda = E*nu / ((1+nu)*(1-2*nu))`, `mu = E / (2*(1+nu))`.
    fn lame_one(e: f64, nu: f64) -> (f64, f64) {
        let lambda = e * nu / ((1.0 + nu) * (1.0 - 2.0 * nu));
        let mu = e / (2.0 * (1.0 + nu));
        (lambda, mu)
    }

    /// One (lambda, mu) Lamé pair per zone (length `num_zones`), broadcasting whichever of
    /// `e`/`nu` is `Constant`, and validating every zone's pair.
    fn lame_per_zone(&self, num_zones: usize) -> anyhow::Result<Vec<(f64, f64)>> {
        let e_vals = self.e.resolve(num_zones, "E")?;
        let nu_vals = self.nu.resolve(num_zones, "nu")?;
        e_vals
            .iter()
            .zip(&nu_vals)
            .enumerate()
            .map(|(zone, (&e, &nu))| {
                Self::validate_one(e, nu).with_context(|| format!("zone {}", zone + 1))?;
                Ok(Self::lame_one(e, nu))
            })
            .collect()
    }
}

/// `elasaniso` (orthotropic elasticity) coefficients: `PROPS(1..9)` are the upper triangle of
/// the local-basis stiffness matrix, `PROPS(10:12)`/`PROPS(13:15)` are the two local basis
/// vectors (`e1`, `e2` — `elasaniso.f90` normalizes `e1`, then Gram-Schmidt-orthogonalizes and
/// normalizes `e2` against it, then derives `e3` itself).
#[derive(Clone)]
pub(crate) struct OrthotropicCoeffs {
    pub(crate) c11: ZoneValue,
    pub(crate) c12: ZoneValue,
    pub(crate) c13: ZoneValue,
    pub(crate) c22: ZoneValue,
    pub(crate) c23: ZoneValue,
    pub(crate) c33: ZoneValue,
    pub(crate) c44: ZoneValue,
    pub(crate) c55: ZoneValue,
    pub(crate) c66: ZoneValue,
    pub(crate) e1: ZoneVec3,
    pub(crate) e2: ZoneVec3,
}

impl Default for OrthotropicCoeffs {
    fn default() -> Self {
        Self {
            c11: ZoneValue::default(),
            c12: ZoneValue::default(),
            c13: ZoneValue::default(),
            c22: ZoneValue::default(),
            c23: ZoneValue::default(),
            c33: ZoneValue::default(),
            c44: ZoneValue::default(),
            c55: ZoneValue::default(),
            c66: ZoneValue::default(),
            // Aligned with the global axes by default — a valid, orthonormal starting basis.
            e1: ZoneVec3::Constant([1.0, 0.0, 0.0]),
            e2: ZoneVec3::Constant([0.0, 1.0, 0.0]),
        }
    }
}

/// One `<Material numM="...">` block's worth of GUI state: which law, and (regardless of
/// which is currently selected) storage for both implemented laws' coefficients, so
/// switching the law dropdown back and forth doesn't discard what was typed in.
pub(crate) struct MaterialEntry {
    /// AMITEX's 1-based material number — see `detect_material_ids`.
    pub(crate) num_m: usize,
    /// The raw value this material's voxels have in `material_id_vtk`, shown alongside
    /// `num_m` so the mapping back to the source file is visible.
    pub(crate) vtk_id: f64,
    /// This material's zone count, from `detect_zone_ids` against the zone-ID VTK (or `1` if
    /// no zone-ID VTK is selected — AMITEX assumes one zone per material in that case). Drives
    /// how many rows each coefficient's "per zone" input shows.
    pub(crate) num_zones: usize,
    pub(crate) law: LawKind,
    pub(crate) isotropic: IsotropicCoeffs,
    pub(crate) orthotropic: OrthotropicCoeffs,
}

/// Distinct material IDs in `grid`, paired with the AMITEX `numM` each maps to, in ascending
/// `numM` order. Enforces AMITEX's actual numbering rule (`read_geom.f90`): the minimum ID
/// must be exactly 0 or 1 (re-indexed to 1-based if it's 0), and IDs must be contiguous — a
/// gap would mean `mat.xml` needs a `<Material>` block for every integer in the gap too
/// (`material_mod.f90` hard-errors if the `<Material>` count doesn't exactly equal
/// `max(numM)`), which isn't something a per-material GUI editor can sensibly ask for.
pub(crate) fn detect_material_ids(grid: &VtkGrid) -> anyhow::Result<Vec<(f64, usize)>> {
    validate_contiguous_ids(distinct_sorted_values(&grid.data), "material ID")
}

/// Distinct zone IDs found among `material_grid`'s voxels that belong to `material_vtk_id`,
/// paired with the AMITEX `numZ` each maps to. Mirrors `detect_material_ids`'s numbering rule,
/// but scoped to one material's voxels: AMITEX numbers zones locally within each material (per
/// https://amitexfftp.github.io/AMITEX/user_guide/input_files.html — "Within a given material,
/// all the voxels with a common zoneID value define a zone"), so zone 1 in material A and zone
/// 1 in material B are unrelated and independently numbered starting at 1.
pub(crate) fn detect_zone_ids(
    material_grid: &VtkGrid,
    zone_grid: &VtkGrid,
    material_vtk_id: f64,
) -> anyhow::Result<Vec<(f64, usize)>> {
    if material_grid.data.len() != zone_grid.data.len() {
        bail!(
            "material ID VTK ({} voxels) and zone ID VTK ({} voxels) have different voxel counts",
            material_grid.data.len(),
            zone_grid.data.len()
        );
    }
    let zone_values: Vec<f64> = material_grid
        .data
        .iter()
        .zip(&zone_grid.data)
        .filter(|(m, _)| **m == material_vtk_id)
        .map(|(_, z)| *z)
        .collect();
    validate_contiguous_ids(distinct_sorted_values(&zone_values), "zone ID")
}

fn validate_contiguous_ids(ids: Vec<f64>, kind: &str) -> anyhow::Result<Vec<(f64, usize)>> {
    let Some(&min) = ids.first() else {
        bail!("no voxels found for this {kind}");
    };
    if min != 0.0 && min != 1.0 {
        bail!("{kind}s must start at 0 or 1 (AMITEX requirement) — found minimum {min}");
    }
    if let Some(&bad) = ids.iter().find(|v| v.fract() != 0.0) {
        bail!("{kind}s must be integers — found {bad}");
    }
    let max = *ids.last().unwrap();
    let expected_count = (max - min) as usize + 1;
    if ids.len() != expected_count {
        bail!(
            "{kind}s must be contiguous with no gaps — found {} distinct value(s) but the range \
             {min}..={max} implies {expected_count}",
            ids.len()
        );
    }
    let offset = if min == 0.0 { 1 } else { 0 };
    Ok(ids.into_iter().map(|id| (id, id as usize + offset)).collect())
}

/// Reference-medium Lamé coefficients for the FFT scheme's convergence (not a real material —
/// affects iteration count, not physical accuracy). Follows AMITEX's documented rule
/// (Moulinec): `X0 = (min(X) + max(X)) / 2` for `X` = lambda or mu, across every zone of every
/// material. Orthotropic materials don't have a single lambda/mu, so their contribution uses an
/// approximation (average off-diagonal term standing in for lambda, average shear term
/// standing in for mu) — this only affects convergence speed, not the computed result.
fn reference_lambda_mu(materials: &[MaterialEntry]) -> anyhow::Result<(f64, f64)> {
    let mut lambdas = Vec::new();
    let mut mus = Vec::new();
    for m in materials {
        match m.law {
            LawKind::ElasticIsotropic => {
                for (lambda, mu) in m.isotropic.lame_per_zone(m.num_zones)? {
                    lambdas.push(lambda);
                    mus.push(mu);
                }
            }
            LawKind::ElasticOrthotropic => {
                let c = &m.orthotropic;
                let c12 = c.c12.resolve(m.num_zones, "C12")?;
                let c13 = c.c13.resolve(m.num_zones, "C13")?;
                let c23 = c.c23.resolve(m.num_zones, "C23")?;
                let c44 = c.c44.resolve(m.num_zones, "C44")?;
                let c55 = c.c55.resolve(m.num_zones, "C55")?;
                let c66 = c.c66.resolve(m.num_zones, "C66")?;
                for zone in 0..m.num_zones {
                    lambdas.push((c12[zone] + c13[zone] + c23[zone]) / 3.0);
                    mus.push((c44[zone] + c55[zone] + c66[zone]) / 3.0);
                }
            }
            _ => {}
        }
    }
    let bounds = |values: &[f64]| {
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (min + max) / 2.0
    };
    Ok((bounds(&lambdas), bounds(&mus)))
}

/// Writes one scalar `Coeff` — `Type="Constant"` if `value` is a single number, or
/// `Type="Constant_Zone"` with one `<Zone numZ=".." Value=".."/>` per zone otherwise.
fn push_coeff(xml: &mut String, index: usize, comment: &str, value: &ZoneValue, num_zones: usize) -> anyhow::Result<()> {
    match value {
        ZoneValue::Constant(v) => {
            xml.push_str(&format!(
                "    <Coeff Index=\"{index}\" Type=\"Constant\" Value=\"{v}\"/> <!-- {comment} -->\n"
            ));
        }
        ZoneValue::PerZone(_) => {
            let vals = value.resolve(num_zones, comment)?;
            xml.push_str(&format!("    <Coeff Index=\"{index}\" Type=\"Constant_Zone\"> <!-- {comment} -->\n"));
            for (zone, v) in vals.iter().enumerate() {
                xml.push_str(&format!("        <Zone numZ=\"{}\" Value=\"{v}\"/>\n", zone + 1));
            }
            xml.push_str("    </Coeff>\n");
        }
    }
    Ok(())
}

/// Writes one Lamé coefficient (`lambda` or `mu`) from an already-resolved per-zone value
/// list — `elasiso`'s coefficients aren't stored as `ZoneValue` themselves (they're derived
/// from `E`/`nu` via `lame_per_zone`), but the generated XML follows the same Constant vs.
/// Constant_Zone rule as any other coefficient.
fn push_derived_coeff(xml: &mut String, index: usize, comment: &str, values: &[f64]) {
    if values.len() == 1 {
        xml.push_str(&format!(
            "    <Coeff Index=\"{index}\" Type=\"Constant\" Value=\"{}\"/> <!-- {comment} -->\n",
            values[0]
        ));
    } else {
        xml.push_str(&format!("    <Coeff Index=\"{index}\" Type=\"Constant_Zone\"> <!-- {comment} -->\n"));
        for (zone, v) in values.iter().enumerate() {
            xml.push_str(&format!("        <Zone numZ=\"{}\" Value=\"{v}\"/>\n", zone + 1));
        }
        xml.push_str("    </Coeff>\n");
    }
}

/// Writes a 3-component vector coefficient (`e1`/`e2`) as 3 consecutive `Coeff` indices,
/// starting at `start_index`.
fn push_vec3(
    xml: &mut String,
    start_index: usize,
    label: &str,
    value: &ZoneVec3,
    num_zones: usize,
) -> anyhow::Result<()> {
    match value {
        ZoneVec3::Constant(v) => {
            for (axis, comp) in v.iter().enumerate() {
                xml.push_str(&format!(
                    "    <Coeff Index=\"{}\" Type=\"Constant\" Value=\"{comp}\"/> <!-- {label}[{axis}] -->\n",
                    start_index + axis
                ));
            }
        }
        ZoneVec3::PerZone(_) => {
            let vecs = value.resolve(num_zones, label)?;
            for axis in 0..3 {
                xml.push_str(&format!(
                    "    <Coeff Index=\"{}\" Type=\"Constant_Zone\"> <!-- {label}[{axis}] -->\n",
                    start_index + axis
                ));
                for (zone, v) in vecs.iter().enumerate() {
                    xml.push_str(&format!("        <Zone numZ=\"{}\" Value=\"{}\"/>\n", zone + 1, v[axis]));
                }
                xml.push_str("    </Coeff>\n");
            }
        }
    }
    Ok(())
}

/// Generates `mat.xml` from the entered materials, matching the structure/style of the
/// legacy hand-written `mat.xml` (see `20260716142000/mat.xml`): a `Reference_Material`
/// followed by one commented `<Material numM="...">` block per material, in `numM` order.
pub(crate) fn generate_mat_xml(materials: &[MaterialEntry]) -> anyhow::Result<String> {
    if materials.is_empty() {
        bail!("no materials defined — select a Material ID VTK file first");
    }
    if let Some(unimplemented) = materials.iter().find(|m| !m.law.implemented()) {
        bail!(
            "material {} uses \"{}\", which isn't implemented yet — pick a different law",
            unimplemented.num_m,
            unimplemented.law.label()
        );
    }

    let mut sorted: Vec<&MaterialEntry> = materials.iter().collect();
    sorted.sort_by_key(|m| m.num_m);
    for (i, m) in sorted.iter().enumerate() {
        if m.num_m != i + 1 {
            bail!(
                "material numbers must be contiguous starting at 1 — found numM={} at position {}",
                m.num_m,
                i + 1
            );
        }
    }

    let (lambda0, mu0) = reference_lambda_mu(materials)?;

    let mut xml = String::new();
    xml.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    xml.push_str("<Materials>\n\n");
    xml.push_str("<!-- REFERENCE MATERIAL (FFT scheme convergence only, not a real material) -->\n");
    xml.push_str(&format!("<Reference_Material Lambda0=\"{lambda0}\" Mu0=\"{mu0}\" />\n\n"));

    for m in sorted {
        xml.push_str(&format!("<!-- MATERIAL {} -->\n", m.num_m));
        match m.law {
            LawKind::ElasticIsotropic => {
                let pairs = m
                    .isotropic
                    .lame_per_zone(m.num_zones)
                    .with_context(|| format!("material {}", m.num_m))?;
                xml.push_str(&format!(
                    "<Material numM=\"{}\" Lib=\"\" Law=\"{}\">\n",
                    m.num_m,
                    m.law.amitex_name()
                ));
                let lambdas: Vec<f64> = pairs.iter().map(|(l, _)| *l).collect();
                let mus: Vec<f64> = pairs.iter().map(|(_, mu)| *mu).collect();
                push_derived_coeff(&mut xml, 1, "lambda", &lambdas);
                push_derived_coeff(&mut xml, 2, "mu", &mus);
                xml.push_str("</Material>\n\n");
            }
            LawKind::ElasticOrthotropic => {
                let c = &m.orthotropic;
                xml.push_str(&format!(
                    "<Material numM=\"{}\" Lib=\"\" Law=\"{}\">\n",
                    m.num_m,
                    m.law.amitex_name()
                ));
                let stiffness = [
                    (1, "C11", &c.c11),
                    (2, "C12", &c.c12),
                    (3, "C13", &c.c13),
                    (4, "C22", &c.c22),
                    (5, "C23", &c.c23),
                    (6, "C33", &c.c33),
                    (7, "C44", &c.c44),
                    (8, "C55", &c.c55),
                    (9, "C66", &c.c66),
                ];
                for (index, label, value) in stiffness {
                    push_coeff(&mut xml, index, label, value, m.num_zones)
                        .with_context(|| format!("material {}", m.num_m))?;
                }
                push_vec3(&mut xml, 10, "e1", &c.e1, m.num_zones).with_context(|| format!("material {}", m.num_m))?;
                push_vec3(&mut xml, 13, "e2", &c.e2, m.num_zones).with_context(|| format!("material {}", m.num_m))?;
                xml.push_str("</Material>\n\n");
            }
            // Filtered out above.
            _ => unreachable!(),
        }
    }
    xml.push_str("</Materials>\n");
    Ok(xml)
}
