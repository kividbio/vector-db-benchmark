//! Server capabilities of a KiviDB instance that change what the `kividb`
//! engine may safely do, decided from the version the server reports.
//!
//! Lives in the library (not `src/bin/`) so the version gate is unit-tested:
//! a gate that silently opens on an older server would index a multi-valued
//! field into a TAG that matches nothing and publish recall 0.0 for a filter
//! that was never applied (the failure `validate_dataset_support` exists to
//! prevent). A gate that silently stays shut on a NEWER server is just as
//! wrong, the other way round: from 1.0.5 a bare `TAG` splits on `,` (see
//! [`TagSemantics`]), so a scalar keyword value containing a comma would stop
//! matching itself.

/// The `major.minor.patch` core of a KiviDB version.
pub type VersionCore = (u32, u32, u32);

/// A parsed `kividb_version`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KividbVersion {
    pub core: VersionCore,
    /// Whether the reported value carried a SemVer pre-release suffix
    /// (`1.0.5-rc1`). Recorded, but NOT used to order versions: see
    /// [`TAG_SEPARATOR_MIN_VERSION`].
    pub prerelease: bool,
}

/// First KiviDB version whose TAG fields follow RediSearch semantics: `FT.CREATE`
/// accepts `SEPARATOR` and splits stored values on it, and a bare `TAG` (no
/// `SEPARATOR`) splits on RediSearch's default `,`.
///
/// Measured on every published image around the boundary, by declaring a bare
/// TAG and a `TAG SEPARATOR ;`, storing `Smith, John` / `a;b;c`, and querying
/// `@s:{Smith}`, `@s:{Smith, John}` and `@l:{b}`:
///
/// | `kividb_version`                    | `SEPARATOR`              | bare TAG        |
/// |-------------------------------------|--------------------------|-----------------|
/// | 1.0.2, 1.0.3, 1.0.4-rc3, 1.0.4      | `ERR unknown field type` | atomic          |
/// | 1.0.5-rc1, 1.0.5, 1.0.6-rc4         | accepted, splits         | splits on `,`   |
///
/// The pre-releases behave like their own release, so a pre-release is gated
/// on its core version (`1.0.5-rc1` is >= 1.0.5 here, unlike strict SemVer
/// precedence). Treating it as older would put a `1.0.5-rc1` server on the bare
/// TAG declaration, which that server splits on `,`.
pub const TAG_SEPARATOR_MIN_VERSION: VersionCore = (1, 0, 5);

/// How a server treats TAG fields, which decides how the engine must declare
/// them for an exact whole-value match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagSemantics {
    /// Before 1.0.5: `SEPARATOR` is rejected and a bare TAG value is ONE atomic
    /// token. Scalar fields are declared bare; a multi-valued field cannot be
    /// indexed at all (`@f:{b}` never matches a stored `a;b;c`).
    Atomic,
    /// From 1.0.5: RediSearch semantics. Every TAG field must be declared
    /// `SEPARATOR ;` (as redis.rs does): bare, a stored `Smith, John` is split
    /// into `Smith` / `John` and `@f:{Smith, John}` matches nothing.
    Separator,
}

/// What a KiviDB server reported about itself in `INFO server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KividbServer {
    /// The raw `kividb_version` value, if the field was present.
    pub reported: Option<String>,
    /// `reported`, parsed. `None` when absent or not a recognised version.
    pub version: Option<KividbVersion>,
}

impl KividbServer {
    /// Read the server's identity out of an `INFO` (or `INFO server`) reply.
    pub fn from_info(info: &str) -> Self {
        let reported = raw_kividb_version(info).map(str::to_string);
        let version = reported.as_deref().and_then(parse_version);
        Self { reported, version }
    }

    /// The server's TAG semantics, or `None` when its version is unknown. An
    /// unknown server gets NO TAG declaration: neither choice is safe without
    /// knowing which semantics it has (bare splits on `,` on a new server;
    /// `SEPARATOR` is rejected on an old one, and a multi-valued field cannot be
    /// indexed there at all), so the engine refuses TAG-bearing schemas instead.
    pub fn tag_semantics(&self) -> Option<TagSemantics> {
        self.version.map(|v| {
            if v.core >= TAG_SEPARATOR_MIN_VERSION {
                TagSemantics::Separator
            } else {
                TagSemantics::Atomic
            }
        })
    }

    /// One clause naming what the server reported, for error messages, so
    /// "why is my server rejected?" is answerable from the message alone.
    pub fn describe(&self) -> String {
        match (&self.reported, &self.version) {
            (None, _) => "`INFO server` reported no `kividb_version` field".to_string(),
            (Some(raw), None) => format!(
                "`INFO server` reported `kividb_version:{raw}`, which is not a \
                 `major.minor.patch[-pre][+build]` version"
            ),
            (Some(raw), Some(_)) => format!("`INFO server` reported `kividb_version:{raw}`"),
        }
    }
}

/// The raw `kividb_version` value in an `INFO` reply, trimmed.
fn raw_kividb_version(info: &str) -> Option<&str> {
    info.lines()
        .find_map(|line| line.trim().strip_prefix("kividb_version:"))
        .map(str::trim)
}

/// Parse `major.minor.patch[-prerelease][+build]` (SemVer shape). Anything else
/// (`1.0`, `v1.0.5`, `1.0.5.1`, `+1.0.5`) is `None`: an unrecognised version is
/// reported as such, never guessed at.
fn parse_version(value: &str) -> Option<KividbVersion> {
    // Build metadata never affects precedence.
    let value = value.split_once('+').map_or(value, |(v, _)| v);
    let (core, prerelease) = match value.split_once('-') {
        Some((core, pre)) => {
            let valid_pre = !pre.is_empty()
                && pre
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
            if !valid_pre {
                return None;
            }
            (core, true)
        }
        None => (value, false),
    };
    // `u32::from_str` accepts a leading `+`, so check digits explicitly.
    let num = |s: &str| -> Option<u32> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let mut parts = core.split('.');
    let major = num(parts.next()?)?;
    let minor = num(parts.next()?)?;
    let patch = num(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some(KividbVersion {
        core: (major, minor, patch),
        prerelease,
    })
}

/// Extract and parse `kividb_version` from an `INFO` (or `INFO server`) reply.
pub fn parse_kividb_version(info: &str) -> Option<KividbVersion> {
    KividbServer::from_info(info).version
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO_105: &str =
        "# Server\r\nredis_version:8.10.0\r\nkividb_version:1.0.5\r\ngcc_version:0.0.0\r\n";

    fn info(v: &str) -> String {
        format!("# Server\r\nkividb_version:{v}\r\n")
    }

    fn semantics(v: &str) -> Option<TagSemantics> {
        KividbServer::from_info(&info(v)).tag_semantics()
    }

    #[test]
    fn parses_the_field_out_of_a_real_info_reply() {
        assert_eq!(
            parse_kividb_version(INFO_105),
            Some(KividbVersion {
                core: (1, 0, 5),
                prerelease: false
            })
        );
    }

    #[test]
    fn does_not_mistake_redis_version_for_kividb_version() {
        // A plain Redis (or a KiviDB build without the field) reports only
        // redis_version; that must read as "unknown", not as 8.10.0.
        let s = KividbServer::from_info("redis_version:8.10.0\r\n");
        assert_eq!(s.version, None);
        assert_eq!(s.tag_semantics(), None);
    }

    #[test]
    fn rejects_malformed_versions() {
        for v in [
            "1.0", "1.0.5.1", "x.y.z", "", "v1.0.5", "+1.0.5", "1.+0.5", "1.0.5-", "1.0.5-r c",
        ] {
            assert_eq!(parse_kividb_version(&info(v)), None, "{v:?} must not parse");
            assert_eq!(semantics(v), None, "{v:?} must read as unknown");
        }
    }

    #[test]
    fn parses_prerelease_and_build_metadata() {
        let v = parse_kividb_version(&info("1.0.6-rc4")).unwrap();
        assert_eq!(v.core, (1, 0, 6));
        assert!(v.prerelease);
        let v = parse_kividb_version(&info("1.0.6+build.7")).unwrap();
        assert_eq!(v.core, (1, 0, 6));
        assert!(!v.prerelease);
    }

    /// The measured boundary (`TAG_SEPARATOR_MIN_VERSION`'s table), one row per
    /// image probed, plus the numeric-ordering traps.
    #[test]
    fn tag_semantics_follow_the_measured_boundary() {
        use TagSemantics::*;
        for (v, want) in [
            ("1.0.2", Atomic),
            ("1.0.3", Atomic),
            ("1.0.4-rc3", Atomic),
            ("1.0.4", Atomic),
            // Measured: 1.0.5-rc1 already splits a bare TAG on `,`.
            ("1.0.5-rc1", Separator),
            ("1.0.5", Separator),
            ("1.0.6-rc4", Separator),
            ("1.1.0", Separator),
            ("2.0.0", Separator),
            // "1.0.10" < "1.0.5" as strings; as a version it is newer.
            ("1.0.10", Separator),
            ("0.10.5", Atomic),
        ] {
            assert_eq!(semantics(v), Some(want), "kividb_version:{v}");
        }
    }

    #[test]
    fn describe_names_what_the_server_reported() {
        let absent = KividbServer::from_info("redis_version:8.10.0\r\n").describe();
        assert!(absent.contains("no `kividb_version`"), "{absent}");
        let garbage = KividbServer::from_info(&info("v1.0.5")).describe();
        assert!(
            garbage.contains("`kividb_version:v1.0.5`") && garbage.contains("not a"),
            "{garbage}"
        );
        let old = KividbServer::from_info(&info("1.0.2")).describe();
        assert!(old.contains("`kividb_version:1.0.2`"), "{old}");
        assert!(!old.contains("not a"), "{old}");
    }
}
