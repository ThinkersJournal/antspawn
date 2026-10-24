// SPDX-License-Identifier: MIT OR Apache-2.0
//! The capabilities a launcher can be certified for.
//!
//! A capability is **off until a test row in the matrix says it is on**. The ids are the stable
//! strings used in job specs and in the matrix file.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

macro_rules! capabilities {
    ($($variant:ident => $id:literal),+ $(,)?) => {
        /// One thing a launcher may or may not be able to do, proven by a test.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Capability { $($variant),+ }

        impl Capability {
            /// Every capability, in declaration order.
            pub const ALL: &'static [Capability] = &[$(Capability::$variant),+];

            /// The stable id used in specs and in the matrix file.
            pub fn id(self) -> &'static str {
                match self { $(Capability::$variant => $id),+ }
            }

            /// The id as the capability engine's type.
            pub fn cap_id(self) -> capant::CapabilityId {
                capant::CapabilityId::new(self.id()).expect("every capability id is a valid CapAnt id")
            }
        }

        impl FromStr for Capability {
            type Err = UnknownCapability;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s { $($id => Ok(Capability::$variant),)+ other => Err(UnknownCapability(other.to_owned())) }
            }
        }
    };
}

capabilities! {
    NoReadOutside => "isolation.no-read-outside",
    NoWriteOutside => "isolation.no-write-outside",
    CleanSlateNextRun => "clean-slate-next-run",
    KillsRunawayProcess => "kills-runaway-process",
    GpuCuda => "gpu.cuda",
    GpuVulkan => "gpu.vulkan",
    NetworkOff => "network.off",
    Concurrent => "concurrent",
}

/// A capability id nobody defined (a typo must not silently mean "no requirement").
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown capability `{0}`")]
pub struct UnknownCapability(pub String);

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

impl Serialize for Capability {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.id())
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_round_trips() {
        for &c in Capability::ALL {
            assert_eq!(c.id().parse::<Capability>().unwrap(), c);
        }
        assert_eq!(Capability::ALL.len(), 8);
    }

    #[test]
    fn every_id_is_a_valid_engine_id() {
        for &c in Capability::ALL {
            assert_eq!(c.cap_id().as_str(), c.id());
        }
    }

    #[test]
    fn a_typo_is_an_error_not_a_missing_requirement() {
        assert!("isolation.no-read-outsid".parse::<Capability>().is_err());
    }
}
