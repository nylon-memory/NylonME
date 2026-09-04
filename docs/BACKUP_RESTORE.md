# Backup & Restore (L2.4)

The engine stores everything under `NYLON_DATA_DIR`:

```text
data/
  graph.snp      # point-in-time snapshot (written by checkpoint)
  wal.log        # write-ahead log: every mutation since the last checkpoint
  audit.jsonl    # append-only audit event stream (L2.3)
```

Recovery is automatic: on startup the engine loads `graph.snp`, then replays
`wal.log` (a torn tail record from a crash is truncated safely). A backup is
therefore just **checkpoint + copy the directory**.

## Online backup (no downtime)

```bash
# 1. flush a fresh snapshot and truncate the WAL
curl -X POST http://<engine>:50052/v1/checkpoint
#    (with auth enabled: -H "x-api-key: <admin-key>"; admin scope required)

# 2. copy the data dir - after checkpoint the WAL is tiny and crash-consistent
tar czf nylonme-backup-$(date +%Y%m%d-%H%M).tgz -C /path/to/data .
```

The engine also checkpoints automatically every `NYLON_CHECKPOINT_SECS`
seconds (default 600, `0` disables), so an unplanned copy of the directory
is still crash-consistent: replay simply truncates a possibly-torn WAL tail,
at most losing the last seconds of writes.

## Restore

```bash
# engine stopped
rm -rf /path/to/data
mkdir -p /path/to/data
tar xzf nylonme-backup-YYYYMMDD-HHMM.tgz -C /path/to/data
# start engine - it loads graph.snp and replays whatever WAL is present
nylon-engine serve 0.0.0.0:50051
```

Verify after restore:

```bash
curl http://<engine>:50052/v1/stats     # nodes/edges match pre-backup counts
```

## Kubernetes (Helm)

Same drill against the PVC:

```bash
kubectl exec deploy/<release>-nylonme -- \
  curl -s -X POST http://localhost:50052/v1/checkpoint
kubectl cp <pod>:/home/nylon/data ./nylonme-backup
# restore: scale to 0, kubectl cp back into the PVC mount, scale to 1
```

## Drill log

2026-09-04 (192.168.1.5): restored a full production copy into a scratch dir
and booted a second engine on alternate ports; node/edge counts and resonance
recall matched the live engine. Exact steps in the internal notes.
