# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 X.AI Corp.
import json
import pathlib
import time

TREE_DEK_NAME = "_DEK"
TREE_DEK_CLAIM_NAME = "_DEK.claim"
PUBLISH_TIMEOUT_SECS = 120.0


def _read_wrapped_dek(dek_path: pathlib.Path) -> dict | None:
    try:
        entry = json.loads(dek_path.read_text())
        return entry if entry.get("wrapped") else None
    except (OSError, ValueError):
        return None


def publish_tree_dek(path: pathlib.Path, kms_client) -> tuple[bytes, str, dict]:
    import xai_kms

    dek_path = path / TREE_DEK_NAME
    claim = path / TREE_DEK_CLAIM_NAME

    deadline = time.monotonic() + PUBLISH_TIMEOUT_SECS
    while (entry := _read_wrapped_dek(dek_path)) is None:
        try:
            claim.open("x").close()
            won_claim = True
        except FileExistsError:
            won_claim = False
        if won_claim:
            try:
                if _read_wrapped_dek(dek_path) is None:
                    xai_kms.nfs.write_shared_dek(kms_client, dek_path)
            except BaseException:
                if _read_wrapped_dek(dek_path) is None:
                    claim.unlink(missing_ok=True)
                raise
            continue
        if time.monotonic() >= deadline:
            raise RuntimeError(
                f"timed out waiting for the wrapped DEK at {dek_path}; its minter "
                "(the rank holding the .claim marker) likely died before publishing"
            )
        time.sleep(0.1)
    raw = bytes(xai_kms.nfs.unwrap_shared_dek(kms_client, dek_path))
    return raw, entry["wrapped"], entry.get("context") or {}
