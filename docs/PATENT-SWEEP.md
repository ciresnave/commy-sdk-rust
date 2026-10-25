# Patent sweep: commy-sdk-rust 0.17.1 @ 6aaa245
Date: 2026-10-25; Run by: commy-sdk-rust lane, Claude Sonnet 5.5; Method: AI-assisted search and read; NOT legal advice; NOT a freedom-to-operate opinion.

This is a due-diligence read under `C:\Projects\PATENT-SWEEP-PROCESS.md` (CireSnave, 2026-10-25, board 286). It lowers the chance of shipping something that plainly overlaps an existing patent; it does not clear the crate.

## Ideas swept
Mechanisms in this crate (the wire protocol is `commy-wire`'s, the change finder is `dirty-ranges`'; both are listed because this crate uses them).
1. A client proves it shares the server's filesystem by reading a nonce from a `0600` side file outside the service file and echoing it to the server over the authenticated connection; any failure falls back to range frames over WSS.
2. A shared file of bytes is kept in sync by diffing the mapping against a baseline and sending only the changed ranges, batched (at most 1024 per message), each batch answered by an acknowledgement carrying a sequence number.
3. An opt-in write trap write-protects the mapped pages so a write is noticed when it happens (`dirty-ranges`; `Session::arm_shared_trap`).
4. Inbound updates are applied through a second, SDK-only view of the same file and to the baseline in one step, so remote writes are never reported back as local changes.
5. A frame whose sequence number is lower than the last applied for that file is dropped as stale.
6. Replacing a shared file (resize or revocation) runs drain, then commit or abort: writes are refused, pending writes are flushed into the old file, a drained notice is sent, and the client switches to the new file only on commit.
7. After a revocation, a revoked client's unflushed local writes are written into the new file and flushed.
8. A new file is filled by sending only its non-zero runs (the server's file starts zero filled).
9. A remote mirror: the whole file is fetched once and kept current by range frames.
10. Authentication is per tenant and additive on one connection, with a tenant-bound session handle.

## Queries (source, query, date)
All 2026-10-25, web search restricted to nothing; results were Google Patents and USPTO full-text pages plus general web.
- Idea 1: "patent verify client shares filesystem with server nonce file read echo over authenticated connection shared memory mapping"
- Idea 3: "patent write-protect memory pages SIGSEGV handler track dirty pages memory mapped file synchronize changed ranges"; "patent protected memory region dirty page tracking page fault handler user space library incremental checkpoint modified pages"
- Ideas 2, 4, 5, 9: "patent replicate shared memory region by comparing against baseline snapshot send only changed byte ranges to server sequence number acknowledgement"
- Ideas 6, 8: "patent file rotation drain pending writes two-phase commit abort replace shared file clients switch to new file"
- Idea 7: "patent client revoked access unflushed local writes merged into replacement file after key revocation shared memory file"
- Idea 10 was not searched separately (per-tenant login on one connection is generic authentication).

## Hits
| # | patent | title | assignee | status | claim(s) read | overlap | our difference | verdict |
|---|---|---|---|---|---|---|---|---|
| 1 | US 8,127,174 B1 | Method and apparatus for performing transparent in-memory checkpointing | Symantec / Cohesity (per Google) | Active, adjusted expiry 2030-01-27 | independent 1, 21 (9, 15 restate 1) | Claim 21: "set all portions to read-only", track faults on writes, "identify faulted portions as modified", copy and replicate them to a backup server. Same mechanism as idea 3. | Ours is an opt-in trap in a separate crate (`dirty-ranges`) over a file mapping shared by several live clients; there is no checkpoint, no backup server, no checkpoint data mapped into a region. The elements are met in mechanism, differ in context. | **needs-review** |
| 2 | US 11,249,660 B2 (and continuation 11,698,737) | Low-latency shared memory channel across address spaces | VMware | Expired - fee related (11,249,660) | independent 1, 7, 13 (read in part; claim 1 truncated by the page) | Shared memory plus authentication information sent to a server. | Claims require a guest VM, a hypervisor filesystem server and guest-virtual / guest-physical / machine page-number mapping; none exist here. No nonce side file. 11,698,737 was not read. | clear (idea 1) |
| 3 | US 7,958,329 B2 | Hybrid replicated shared memory | Waratek | Active, adjusted expiry 2030-02-11 | independent 1, 5, 6, 10, 14 | Sends content changes in a replicated region to all other computers. | Every independent claim recites partitioning each computer's memory into replicated and non-replicated regions and moving that boundary over time. We have no such partition and do not resize a boundary. | clear (ideas 2, 9) |
| 4 | US 11,677,567 B2 | Validating shared files | Citrix | Active, expires 2041-04-17 | independent 1, 8 | Shared files between clients. | Claims are hash-value signing and validation with a document key; no memory mapping, nothing in the data path. | clear |
| 5 | US 9,256,496 B1, US 9,792,182 B2 | Hybrid kernel/user-space checkpointing; checkpoint generation | (not recorded) | not checked | not read | Dirty-page tracking for checkpoints. | Appeared in results only; not read, no claim analysis. | **not assessed** |
| 6 | US 8,880,473, US 10,310,754 | Storage checkpointing for independent applications | (not recorded) | not checked | not read | Handles EFAULT from kernel writes to read-only pages. | Not read. Our trap has the matching limit (nothing may read from the OS straight into the mapping). | **not assessed** |

Ideas 4, 5, 7 and 8 returned no patent that looked relevant (the 2-phase-commit and write-back results for ideas 6 and 7 were general transaction-processing patents, and kernel mail or documentation pages, not on-point claims). That is an absence of hits from a few queries, not a finding of freedom.

## Needs review (to the PM)
- **US 8,127,174 claim 21 vs idea 3 (write trap).** The elements "set portions read-only, record write faults, treat faulted portions as modified, copy and replicate" are what `dirty-ranges` does when armed. The patent is in checkpointing and backup-server replication, and the trap is opt-in and off by default (the default is a baseline scan, which does not match claim 21). The trap code is in the `dirty-ranges` crate, so the same hit belongs on its sweep; the SDK only exposes `arm_shared_trap`. Needs a human or attorney decision on whether the context difference is enough, or whether the trap is gated or documented as at-your-own-risk until the patent expires (2030-01-27 per Google, adjusted; unverified).

## Limitations
- AI-assisted, web search only; search engine results, not the USPTO or EPO databases directly. Coverage is a handful of queries per idea, not an exhaustive class search (no CPC-filtered search, no EPO/WIPO runs, no inventor or assignee sweeps).
- Several claims were read through Google Patents text that was truncated or scanned without a text layer; where claims could not be read the row says so. Hits 5 and 6 were not read at all.
- Legal status and expiry come from Google Patents and are labelled by it as assumptions, not legal conclusions.
- Only this crate's own ideas were swept. The server (`commy`) and `dirty-ranges` and `commy-wire` have their own records.
- Not legal advice and not a freedom-to-operate opinion.
