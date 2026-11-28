// SPDX-License-Identifier: MIT
//! Reading a tenant's audit log (`ReadAudit` / `AuditEvents`, commy-wire `spec/audit.md`).

use crate::error::{CommyError, Result};
use commy_wire::{AuditEntry, AuditEvents, AuditHashError, AuditHead};

/// One page of a tenant's audit events. Every entry was recomputed with
/// [`commy_wire::verify_audit_entry`] for that tenant, and the page's `seq` values are strictly
/// ascending and past the requested `after`.
///
/// This is hash consistency, not proof against the server: an unkeyed hash can be recomputed by
/// whoever edits an entry, a tenant's view is not a contiguous chain (so entries cannot be linked
/// to one another or to `head`), and the log can be cut short. Record `head` somewhere the server
/// cannot write to notice that. A v2 entry's hash is bound to the tenant it is verified for; a
/// legacy v1 hash carries no tenant, so for those the tenant scoping is the server's word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditPage {
    /// The tenant's events, oldest first. `seq` values are not consecutive in a tenant's view.
    pub entries: Vec<AuditEntry>,
    /// The newest chained event of the whole log (carries no tenant data), if the server has one.
    pub head: Option<AuditHead>,
    /// True when `limit` cut the list: ask again with `after` set to the last entry's `seq`.
    pub more: bool,
}

impl AuditPage {
    /// Verify every entry of `reply` for `tenant_id` and build the page. One entry that does not
    /// verify, names an unknown hash formula, or carries no hash fails the whole page, as does a
    /// `seq` that is not strictly ascending or not greater than `after`.
    pub(crate) fn from_reply(
        reply: AuditEvents,
        tenant_id: &str,
        after: Option<u64>,
    ) -> Result<Self> {
        let mut floor = after;
        for e in &reply.entries {
            if floor.is_some_and(|f| e.seq <= f) {
                return Err(CommyError::InvalidMessage(format!(
                    "audit entry seq {} is out of order or not after the requested cursor",
                    e.seq
                )));
            }
            floor = Some(e.seq);
            commy_wire::verify_audit_entry(e, Some(tenant_id)).map_err(|why| {
                CommyError::InvalidMessage(match why {
                    AuditHashError::Mismatch => {
                        format!("audit entry seq {} does not match its hash", e.seq)
                    }
                    AuditHashError::UnsupportedVersion => {
                        format!("audit entry seq {} has an unknown hash formula", e.seq)
                    }
                    AuditHashError::NotChained => {
                        format!(
                            "audit entry seq {} carries no hash and cannot be verified",
                            e.seq
                        )
                    }
                })
            })?;
        }
        Ok(AuditPage {
            entries: reply.entries,
            head: reply.head,
            more: reply.more,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commy_wire::{audit_hash_v1, audit_hash_v2};

    fn v2(seq: u64, prev: &str, tenant: &str, line: &str) -> AuditEntry {
        AuditEntry {
            seq,
            ts_ms: 1_000 + seq,
            line: line.to_string(),
            prev: prev.to_string(),
            hash: audit_hash_v2(seq, 1_000 + seq, prev, Some(tenant), line),
        }
    }

    fn reply(entries: Vec<AuditEntry>) -> AuditEvents {
        AuditEvents {
            request_id: "r".into(),
            entries,
            head: Some(AuditHead {
                seq: 99,
                hash: "h".into(),
            }),
            more: true,
        }
    }

    #[test]
    fn verified_v2_and_v1_entries_make_a_page() {
        let a = v2(3, "", "acme", "grant");
        let b = AuditEntry {
            seq: 5,
            ts_ms: 7,
            line: "old".into(),
            prev: a.hash.clone(),
            hash: audit_hash_v1(5, 7, &a.hash, "old"),
        };
        let page = AuditPage::from_reply(reply(vec![a.clone(), b.clone()]), "acme", None).unwrap();
        assert_eq!(page.entries, [a, b]);
        assert!(page.more);
        assert_eq!(page.head.unwrap().seq, 99);
    }

    #[test]
    fn an_entry_verified_for_another_tenant_is_refused() {
        // Positive control: the same entry passes for its own tenant.
        let e = v2(3, "", "acme", "grant");
        assert!(AuditPage::from_reply(reply(vec![e.clone()]), "acme", None).is_ok());
        assert!(AuditPage::from_reply(reply(vec![e]), "globex", None).is_err());
    }

    #[test]
    fn an_edited_line_fails_the_whole_page() {
        let good = v2(1, "", "acme", "one");
        let mut bad = v2(2, &good.hash, "acme", "two");
        bad.line = "tampered".into();
        let err = AuditPage::from_reply(reply(vec![good, bad]), "acme", None).unwrap_err();
        assert!(
            matches!(&err, CommyError::InvalidMessage(m) if m.contains("seq 2")),
            "{err}"
        );
    }

    #[test]
    fn an_unknown_hash_prefix_is_refused_never_skipped() {
        let mut e = v2(1, "", "acme", "one");
        e.hash = format!("v3:{}", "a".repeat(64));
        let err = AuditPage::from_reply(reply(vec![e]), "acme", None).unwrap_err();
        assert!(
            matches!(&err, CommyError::InvalidMessage(m) if m.contains("unknown")),
            "{err}"
        );
    }

    #[test]
    fn an_entry_with_no_hash_is_refused() {
        let mut e = v2(1, "", "acme", "one");
        e.hash = String::new();
        assert!(AuditPage::from_reply(reply(vec![e]), "acme", None).is_err());
    }

    #[test]
    fn duplicate_descending_or_stale_sequences_are_refused() {
        let a = v2(3, "", "acme", "one");
        let b = v2(4, &a.hash, "acme", "two");
        // Positive control: ascending and past the cursor is accepted.
        assert!(AuditPage::from_reply(reply(vec![a.clone(), b.clone()]), "acme", Some(2)).is_ok());
        for (entries, after) in [
            (vec![a.clone(), a.clone()], None),
            (vec![b.clone(), a.clone()], None),
            (vec![a.clone(), b.clone()], Some(3)),
        ] {
            let err = AuditPage::from_reply(reply(entries), "acme", after).unwrap_err();
            assert!(
                matches!(&err, CommyError::InvalidMessage(m) if m.contains("out of order")),
                "{err}"
            );
        }
    }

    #[test]
    fn an_empty_page_is_fine() {
        let page = AuditPage::from_reply(
            AuditEvents {
                request_id: "r".into(),
                entries: vec![],
                head: None,
                more: false,
            },
            "acme",
            None,
        )
        .unwrap();
        assert!(page.entries.is_empty() && page.head.is_none() && !page.more);
    }
}
