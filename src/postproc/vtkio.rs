use anyhow::{anyhow, bail, Context};
use std::path::Path;

/// A legacy-VTK `STRUCTURED_POINTS` / `CELL_DATA` scalar field, as written by
/// `amitex_fftp` (see `vtktools.py` in `20260716142000/` for the format this mirrors).
pub(crate) struct VtkGrid {
    pub(crate) nx: usize,
    pub(crate) ny: usize,
    pub(crate) nz: usize,
    pub(crate) dx: f64,
    pub(crate) dy: f64,
    pub(crate) dz: f64,
    pub(crate) varname: String,
    pub(crate) datatype: String,
    /// Cell values in (z, y, x) row-major order, upcast to `f64` regardless of the
    /// on-disk numeric type.
    pub(crate) data: Vec<f64>,
}

fn bytes_per_value(datatype: &str) -> anyhow::Result<usize> {
    Ok(match datatype {
        "unsigned_char" | "char" => 1,
        "unsigned_short" | "short" => 2,
        "unsigned_int" | "int" | "float" => 4,
        "unsigned_long" | "long" | "double" => 8,
        other => bail!("unsupported VTK scalar type: {other}"),
    })
}

fn decode_value(datatype: &str, bytes: &[u8]) -> f64 {
    match datatype {
        "unsigned_char" => bytes[0] as f64,
        "char" => (bytes[0] as i8) as f64,
        "unsigned_short" => u16::from_be_bytes(bytes.try_into().unwrap()) as f64,
        "short" => i16::from_be_bytes(bytes.try_into().unwrap()) as f64,
        "unsigned_int" => u32::from_be_bytes(bytes.try_into().unwrap()) as f64,
        "int" => i32::from_be_bytes(bytes.try_into().unwrap()) as f64,
        "float" => f32::from_be_bytes(bytes.try_into().unwrap()) as f64,
        "unsigned_long" => u64::from_be_bytes(bytes.try_into().unwrap()) as f64,
        "long" => i64::from_be_bytes(bytes.try_into().unwrap()) as f64,
        "double" => f64::from_be_bytes(bytes.try_into().unwrap()),
        other => unreachable!("unsupported VTK scalar type: {other}"),
    }
}

fn encode_value(datatype: &str, value: f64, out: &mut Vec<u8>) {
    match datatype {
        "unsigned_char" => out.push(value as u8),
        "char" => out.push(value as i8 as u8),
        "unsigned_short" => out.extend_from_slice(&(value as u16).to_be_bytes()),
        "short" => out.extend_from_slice(&(value as i16).to_be_bytes()),
        "unsigned_int" => out.extend_from_slice(&(value as u32).to_be_bytes()),
        "int" => out.extend_from_slice(&(value as i32).to_be_bytes()),
        "float" => out.extend_from_slice(&(value as f32).to_be_bytes()),
        "unsigned_long" => out.extend_from_slice(&(value as u64).to_be_bytes()),
        "long" => out.extend_from_slice(&(value as i64).to_be_bytes()),
        "double" => out.extend_from_slice(&value.to_be_bytes()),
        other => unreachable!("unsupported VTK scalar type: {other}"),
    }
}

/// Reads a legacy-VTK `STRUCTURED_POINTS`/`CELL_DATA` scalar file (ASCII or big-endian
/// `BINARY`), matching `vtktools.vtk2numpy`.
pub(crate) fn read_vtk_cell_scalars(path: &Path) -> anyhow::Result<VtkGrid> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;

    // The header is always plain ASCII, newline-terminated, regardless of ASCII/BINARY body.
    let mut lines = Vec::with_capacity(10);
    let mut offset = 0usize;
    while lines.len() < 10 {
        let nl = bytes[offset..]
            .iter()
            .position(|&b| b == b'\n')
            .ok_or_else(|| anyhow!("{}: truncated header", path.display()))?;
        lines.push(std::str::from_utf8(&bytes[offset..offset + nl])?.trim().to_string());
        offset += nl + 1;
    }

    let ascii_or_binary = lines[2].as_str();
    let dims: Vec<i64> = lines[4]
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    let (mut nx, mut ny, mut nz) = (dims[0], dims[1], dims[2]);
    let spacing: Vec<f64> = lines[6]
        .split_whitespace()
        .skip(1)
        .map(str::parse)
        .collect::<Result<_, _>>()?;
    let (dx, dy, dz) = (spacing[0], spacing[1], spacing[2]);

    let mut cell_or_point = lines[7].split_whitespace();
    let kind = cell_or_point.next().ok_or_else(|| anyhow!("missing CELL_DATA/POINT_DATA"))?;
    let n: usize = cell_or_point
        .next()
        .ok_or_else(|| anyhow!("missing data count"))?
        .parse()?;
    if kind == "CELL_DATA" {
        nx -= 1;
        ny -= 1;
        nz -= 1;
    }

    let mut scalars_fields = lines[8].split_whitespace();
    scalars_fields.next(); // "SCALARS"
    let varname = scalars_fields
        .next()
        .ok_or_else(|| anyhow!("missing SCALARS name"))?
        .to_string();
    let datatype = scalars_fields
        .next()
        .ok_or_else(|| anyhow!("missing SCALARS type"))?
        .to_string();

    let data = if ascii_or_binary == "BINARY" {
        let width = bytes_per_value(&datatype)?;
        let payload = &bytes[bytes.len() - n * width..];
        payload
            .chunks_exact(width)
            .map(|chunk| decode_value(&datatype, chunk))
            .collect()
    } else if ascii_or_binary == "ASCII" {
        std::str::from_utf8(&bytes[offset..])?
            .split_whitespace()
            .map(str::parse::<f64>)
            .collect::<Result<_, _>>()?
    } else {
        bail!("{}: unknown format {ascii_or_binary:?}", path.display());
    };

    Ok(VtkGrid {
        nx: nx as usize,
        ny: ny as usize,
        nz: nz as usize,
        dx,
        dy,
        dz,
        varname,
        datatype,
        data,
    })
}

/// Writes a legacy-VTK `STRUCTURED_POINTS`/`CELL_DATA` scalar file, matching
/// `vtktools.numpy2vtk` (always `BINARY`, big-endian).
pub(crate) fn write_vtk_cell_scalars(grid: &VtkGrid, path: &Path) -> anyhow::Result<()> {
    let mut header = String::new();
    header.push_str("# vtk DataFile Version 2.0\n");
    header.push_str("Virtual_DMA\n");
    header.push_str("BINARY\n");
    header.push_str("DATASET STRUCTURED_POINTS\n");
    header.push_str(&format!(
        "DIMENSIONS {} {} {}\n",
        grid.nx + 1,
        grid.ny + 1,
        grid.nz + 1
    ));
    header.push_str("ORIGIN 0.000000e+00 0.000000e+00 0.000000e+00\n");
    header.push_str(&format!(
        "SPACING {:.6e} {:.6e} {:.6e}\n",
        grid.dx, grid.dy, grid.dz
    ));
    header.push_str(&format!("CELL_DATA {}\n", grid.nx * grid.ny * grid.nz));
    header.push_str(&format!("SCALARS {} {}\n", grid.varname, grid.datatype));
    header.push_str("LOOKUP_TABLE default\n");

    let mut out = header.into_bytes();
    out.reserve(grid.data.len() * bytes_per_value(&grid.datatype)?);
    for &value in &grid.data {
        encode_value(&grid.datatype, value, &mut out);
    }

    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

/// Distinct values in `data`, sorted ascending. Shared by the materials viewer's legend
/// coloring and the material-ID/zone-ID detection in `preproc::materials` — both need the
/// same "what distinct IDs does this scalar field contain" enumeration. Deduplicates by bit
/// pattern rather than `==` (irrelevant in practice since these fields are integer-valued,
/// but avoids `f64`'s lack of `Eq`/`Hash`).
pub(crate) fn distinct_sorted_values(data: &[f64]) -> Vec<f64> {
    let mut seen = std::collections::HashSet::new();
    let mut values: Vec<f64> = Vec::new();
    for &v in data {
        if seen.insert(v.to_bits()) {
            values.push(v);
        }
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    values
}
