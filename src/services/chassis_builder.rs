//! Deterministic position-code generation per §9.2.
//!
//! Tractor axle N, single:    H{N}L, H{N}R
//! Tractor axle N, dual:      H{N}LO, H{N}LI, H{N}RI, H{N}RO
//! Trailer axle N, single:    TR{N}L, TR{N}R
//! Trailer axle N, dual:      TR{N}LO, TR{N}LI, TR{N}RI, TR{N}RO
//! Spare N:                   SP{N}

use crate::models::chassis::{AxleType, ChassisSection, PositionDepth, PositionSide};

/// (side, depth, code)
pub struct PositionSpec {
    pub side: PositionSide,
    pub depth: PositionDepth,
    pub code: String,
}

pub fn positions_for_axle(
    section: ChassisSection,
    section_index: i16,
    axle_type: AxleType,
) -> Vec<PositionSpec> {
    let prefix = match section {
        ChassisSection::Tractor => "H",
        ChassisSection::Trailer => "TR",
    };
    let n = section_index;

    match axle_type {
        AxleType::Single => vec![
            PositionSpec {
                side: PositionSide::Left,
                depth: PositionDepth::Single,
                code: format!("{prefix}{n}L"),
            },
            PositionSpec {
                side: PositionSide::Right,
                depth: PositionDepth::Single,
                code: format!("{prefix}{n}R"),
            },
        ],
        AxleType::Dual => vec![
            PositionSpec {
                side: PositionSide::Left,
                depth: PositionDepth::Outer,
                code: format!("{prefix}{n}LO"),
            },
            PositionSpec {
                side: PositionSide::Left,
                depth: PositionDepth::Inner,
                code: format!("{prefix}{n}LI"),
            },
            PositionSpec {
                side: PositionSide::Right,
                depth: PositionDepth::Inner,
                code: format!("{prefix}{n}RI"),
            },
            PositionSpec {
                side: PositionSide::Right,
                depth: PositionDepth::Outer,
                code: format!("{prefix}{n}RO"),
            },
        ],
    }
}

pub fn spare_code(spare_index: i16) -> String {
    format!("SP{}", spare_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tractor_single_codes() {
        let v = positions_for_axle(ChassisSection::Tractor, 1, AxleType::Single);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].code, "H1L");
        assert_eq!(v[1].code, "H1R");
    }

    #[test]
    fn tractor_dual_codes() {
        let v = positions_for_axle(ChassisSection::Tractor, 2, AxleType::Dual);
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].code, "H2LO");
        assert_eq!(v[1].code, "H2LI");
        assert_eq!(v[2].code, "H2RI");
        assert_eq!(v[3].code, "H2RO");
    }

    #[test]
    fn trailer_codes() {
        assert_eq!(
            positions_for_axle(ChassisSection::Trailer, 3, AxleType::Single)[0].code,
            "TR3L"
        );
        assert_eq!(
            positions_for_axle(ChassisSection::Trailer, 1, AxleType::Dual)[3].code,
            "TR1RO"
        );
    }

    #[test]
    fn spare_codes() {
        assert_eq!(spare_code(1), "SP1");
        assert_eq!(spare_code(2), "SP2");
    }
}
