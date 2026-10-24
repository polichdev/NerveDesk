# ADR-009: Folder Sync Engine & Conflict Resolution Strategy

| Field | Value |
|---|---|
| Status | Accepted |
| Date | 2026-10-24 |
| Deciders | NerveDesk core team |

---

## Context

NerveDesk includes an automated bidirectional folder synchronization engine (`FolderSync` on TCP port **48108**). When two PCs share a working directory (e.g. project source code, documents, assets), both sides can create, edit, or delete files independently while connected over the local network.

A deterministic strategy was required to handle:
1. Detecting changes efficiently without burning CPU cycles on continuous polling.
2. Handling conflicting edits when both sides edit the same file simultaneously.
3. Preventing recursive echo loops (e.g. downloading a file triggers an upload event back to the sender).

---

## Decision

We adopted an event-driven **ReadDirectoryChangesW / Notify** architecture combined with a **Last-Write-Wins (LWW) with Conflict Preserved Copies** resolution strategy.

1. **Change Detection**: Windows native directory watchers capture `FILE_ACTION_ADDED`, `MODIFIED`, and `RENAMED` events with a 300 ms debounce timer.
2. **Conflict Resolution**:
   - If both machines modify the same relative path within the sync window, the version with the newer UTC modification timestamp wins the primary filename.
   - The losing copy is **never deleted**. It is automatically saved alongside with a conflict suffix:
     `filename.conflict-<machine-name>-<timestamp>.ext`
3. **Loop Suppression**: Files written by the incoming network synchronization engine are tagged in an in-memory ephemeral hash table to suppress outbound re-triggering.

---

## Consequences

**Positive:**
- Zero data loss: users never lose work due to simultaneous edits.
- Minimal CPU overhead thanks to kernel-driven `ReadDirectoryChangesW` instead of recursive filesystem walks.
- Automatic self-healing when network connectivity resumes after temporary drops.

**Negative / Accepted:**
- Highly concurrent multi-user editing of binary files (e.g. 3D models, databases) produces conflict files that require manual user review.
