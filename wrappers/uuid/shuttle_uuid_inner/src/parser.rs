//! Parsing, which delegates to uuid's.

use crate::{Error, Uuid};
use core::str::FromStr;

impl Uuid {
    /// Parses a `Uuid` from a string of hexadecimal digits with optional hyphens.
    pub fn parse_str(input: &str) -> Result<Uuid, Error> {
        uuid_orig::Uuid::parse_str(input).map(Uuid)
    }

    /// Parses a `Uuid` from a string of hexadecimal digits with optional hyphens, in a `const`
    /// context.
    pub const fn try_parse(input: &str) -> Result<Uuid, Error> {
        match uuid_orig::Uuid::try_parse(input) {
            Ok(uuid) => Ok(Uuid(uuid)),
            Err(e) => Err(e),
        }
    }

    /// Parses a `Uuid` from a string of hexadecimal digits with optional hyphens, in a `const`
    /// context.
    pub const fn try_parse_ascii(input: &[u8]) -> Result<Uuid, Error> {
        match uuid_orig::Uuid::try_parse_ascii(input) {
            Ok(uuid) => Ok(Uuid(uuid)),
            Err(e) => Err(e),
        }
    }
}

impl FromStr for Uuid {
    type Err = Error;

    fn from_str(uuid_str: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(uuid_str)
    }
}

impl TryFrom<&'_ str> for Uuid {
    type Error = Error;

    fn try_from(uuid_str: &'_ str) -> Result<Self, Self::Error> {
        Uuid::parse_str(uuid_str)
    }
}

#[cfg(feature = "std")]
impl TryFrom<String> for Uuid {
    type Error = Error;

    fn try_from(uuid_str: String) -> Result<Self, Self::Error> {
        Uuid::try_from(uuid_str.as_ref())
    }
}
