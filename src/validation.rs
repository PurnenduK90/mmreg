use crate::error::MmregError;

/// One register assertion of a `SafetyProfile`: `(read(addr) & mask) == (expected & mask)`.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "profile",
    derive(serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct ValidationCheck {
    /// Human-readable name of the check (e.g. `"PCAP_DONE"`)
    pub name: String,
    /// Physical address of the status/control register to read
    #[cfg_attr(feature = "profile", serde(deserialize_with = "de::u64"))]
    pub addr: u64,
    /// Bits of the read value that are compared
    #[cfg_attr(feature = "profile", serde(deserialize_with = "de::u32"))]
    pub mask: u32,
    /// Expected value of the masked bits
    #[cfg_attr(feature = "profile", serde(deserialize_with = "de::u32"))]
    pub expected: u32,
}

/// Prerequisite register checks run before a peripheral is mapped (`--validate <profile.toml>`).
///
/// Profile files need the `profile` feature (on by default). Numbers are TOML integers or
/// strings (`"0xFF41_0000"`, `"0d42"`, decimal otherwise):
/// ```toml
/// name = "ZynqMP_AXI_Safety"
///
/// [[checks]]
/// name = "PCAP_DONE"
/// addr = 0xFFCA_3010
/// mask = 0x4
/// expected = 0x4
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "profile",
    derive(serde::Deserialize),
    serde(deny_unknown_fields)
)]
pub struct SafetyProfile {
    /// Profile name
    pub name: String,
    /// Checks, run in order; the first failure stops the run
    pub checks: Vec<ValidationCheck>,
}

impl SafetyProfile {
    /// Loads a profile from a TOML file.
    #[cfg(feature = "profile")]
    pub fn from_file(path: &std::path::Path) -> Result<Self, MmregError> {
        let content =
            std::fs::read_to_string(path).map_err(|e| MmregError::ValidationProfileParse {
                details: format!("failed to read '{}': {}", path.display(), e),
            })?;
        Self::parse(&content)
    }

    /// Parses a profile from TOML. Unknown keys and an empty `checks` list are errors.
    #[cfg(feature = "profile")]
    pub fn parse(toml_str: &str) -> Result<Self, MmregError> {
        let profile: Self =
            toml::from_str(toml_str).map_err(|e| MmregError::ValidationProfileParse {
                details: e.to_string(),
            })?;
        if profile.checks.is_empty() {
            return Err(MmregError::ValidationProfileParse {
                details: "no `[[checks]]` entries".to_string(),
            });
        }
        Ok(profile)
    }

    /// Runs every check with `read_fn` as the register reader.
    pub fn execute_with<F>(&self, mut read_fn: F) -> Result<(), MmregError>
    where
        F: FnMut(u64) -> Result<u32, MmregError>,
    {
        for check in &self.checks {
            let actual = read_fn(check.addr)?;
            if actual & check.mask != check.expected & check.mask {
                return Err(MmregError::ValidationHookFailed {
                    profile_name: self.name.clone(),
                    check_name: check.name.clone(),
                    address: check.addr,
                    mask: check.mask,
                    expected: check.expected,
                    actual,
                });
            }
        }
        Ok(())
    }
}

/// Deserializers accepting a TOML integer or a number string.
#[cfg(feature = "profile")]
mod de {
    use crate::parse::parse_u64_radix;
    use serde::de::{Deserialize, Deserializer, Error};

    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Number {
        Int(u64),
        Str(String),
    }

    pub(super) fn u64<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        match Number::deserialize(d)? {
            Number::Int(v) => Ok(v),
            Number::Str(s) => parse_u64_radix(&s, 10).map_err(D::Error::custom),
        }
    }

    pub(super) fn u32<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        let v = u64(d)?;
        u32::try_from(v).map_err(|_| D::Error::custom(format!("0x{v:X} does not fit in 32 bits")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execute_checks() {
        let profile = SafetyProfile {
            name: "ZynqMP".into(),
            checks: vec![ValidationCheck {
                name: "PCAP_DONE".into(),
                addr: 0xFFCA_3010,
                mask: 0x4,
                expected: 0x4,
            }],
        };
        assert!(profile.execute_with(|_| Ok(0xFFFF_FFF4)).is_ok());
        let err = profile.execute_with(|_| Ok(0)).unwrap_err();
        assert_eq!(err.code(), "E_VALIDATION_HOOK_FAILED");
        assert!(err.to_string().contains("PCAP_DONE"));
        let err = profile
            .execute_with(|address| Err(MmregError::BlueprintUnmapped { address, size: 4 }))
            .unwrap_err();
        assert_eq!(err.code(), "E_BLUEPRINT_UNMAPPED");
    }

    #[cfg(feature = "profile")]
    #[test]
    fn test_parse_toml_profile() {
        let profile = SafetyProfile::parse(
            r#"
# ZynqMP AXI Safety Profile
name = "ZynqMP_AXI_Safety #1"

[[checks]]
name = "PCAP_DONE #bit2"
addr = 0xFFCA_3010
mask = 0x4
expected = 0x4

[[checks]]
name = "ISOLATION_REMOVED"
addr = "0xFF41_0000"
mask = "15"
expected = "0d0"
"#,
        )
        .unwrap();
        assert_eq!(profile.name, "ZynqMP_AXI_Safety #1");
        assert_eq!(profile.checks.len(), 2);
        assert_eq!(profile.checks[0].name, "PCAP_DONE #bit2");
        assert_eq!(profile.checks[1].addr, 0xFF41_0000);
        assert_eq!(profile.checks[1].mask, 0xF);

        let check = "[[checks]]\nname = \"c\"\naddr = 0x1000\nmask = 0x1\nexpected = 0x1\n";
        for bad in [
            format!("name = \"Bad\"\nunknown_key = 42\n{check}"),
            format!("name = \"Bad\"\n{check}typo_field = true\n"),
            "name = \"Bad\"\nchecks = []\n".to_string(),
            "name = \"Bad\"\n".to_string(),
            check.to_string(),
            check.replace("mask = 0x1", "mask = 0x1_0000_0000"),
            check.replace("addr = 0x1000", "addr = \"0x0x10\""),
        ] {
            assert_eq!(
                SafetyProfile::parse(&bad).unwrap_err().code(),
                "E_VALIDATION_PROFILE_PARSE",
                "{bad}"
            );
        }
        assert!(SafetyProfile::from_file(std::path::Path::new("/nonexistent/p.toml")).is_err());
    }
}
