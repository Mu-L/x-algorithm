# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 X.AI Corp.
import contextlib
import ctypes
import fcntl
import functools
import gc
import json
import logging
import math
import os
import pathlib
import re
import time
from typing import Any, Callable

import jax
import tensorstore as ts
from jax.experimental.shard_map import shard_map

from xai_checkpointing import (
    common,
    fix_jax,
)
from xai_checkpointing.tree_util import has_subtree, tree_to_dict

import orbax.checkpoint as ocp

logger = logging.getLogger("checkpointing")
rank_logger = logging.getLogger("rank")

PyTree = common.PyTree

_NODE_SERIALIZE_ENV = "XAI_RESTORE_NODE_SERIALIZE"
_NODE_LOCK_FILE_ENV = "XAI_RESTORE_NODE_LOCK_FILE"


def _restore_node_serialize_enabled() -> bool:
    return os.getenv(_NODE_SERIALIZE_ENV, "1").lower() not in ("0", "", "false")


def _node_lock_path() -> str:
    path = os.getenv(_NODE_LOCK_FILE_ENV)
    if path:
        return path
    if os.path.isdir("/dev/shm"):
        return "/dev/shm/xai_restore_node_lock"
    return "/tmp/xai_restore_node_lock"


class _NodeBatchLock:
    def __init__(self, path: str):
        self._path = path
        self._fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o666)

    def __enter__(self):
        t0 = time.time()
        fcntl.flock(self._fd, fcntl.LOCK_EX)
        waited = time.time() - t0
        if waited > 1.0:
            rank_logger.info(
                "restore node-serialize: waited %.1fs for node lock %s", waited, self._path
            )
        return self

    def __exit__(self, *exc):
        fcntl.flock(self._fd, fcntl.LOCK_UN)

    def close(self):
        try:
            os.close(self._fd)
        except OSError:
            pass


def _release_batch_memory():
    gc.collect()
    try:
        ctypes.CDLL("libc.so.6").malloc_trim(0)
    except Exception:
        pass


def _read_into_shards(
    t: ts.TensorStore,
    array: jax.Array,
    mask: list[bool],
    restricted_domain: ts.IndexDomain | None = None,
):
    memory_kind = array.sharding.memory_kind
    is_cpu = all(d.platform == "cpu" for d in array.sharding.addressable_devices)
    assert memory_kind == "pinned_host" or is_cpu, (
        f"expected pinned_host memory, got {memory_kind!r}"
    )
    devices_indices_map = array.sharding.addressable_devices_indices_map(array.shape)

    for shard, should_load in zip(array.addressable_shards, mask, strict=True):
        if not should_load:
            continue

        index = devices_indices_map[shard.device]
        shard_domain = ts.IndexTransform(input_shape=array.shape)[index].domain

        domain = shard_domain
        if restricted_domain:
            domain = restricted_domain.intersect(shard_domain)

        src = t[domain]

        a = common._unsafe_jax2np(shard.data)
        assert (
            shard.data.addressable_data(0).unsafe_buffer_pointer()
            == a.__array_interface__["data"][0]
        )

        dest = ts.array(a)[ts.d[:].translate_to[shard_domain.origin]][domain]
        yield dest.write(src).commit


def load_checkpoint(
    path: str,
    host_state: PyTree[jax.Array],
    load_mask: PyTree[jax.Array] | None,
    rename: Callable[[str], str] | None = None,
    domains: dict[str, Any] | None = None,
    tag: str | None = None,
    timeout: float = 900.0,
    concurrent_gb: float | None = None,
):
    if tag is None:
        tag = "orbax-ckpt"

    host_state = tree_to_dict(host_state)
    load_mask = tree_to_dict(load_mask)
    domains = tree_to_dict(domains)

    for name, domain in domains.items():
        assert host_state.get(name) is not None, f"Cannot restrict domain of skipped tensor {name}"
        if isinstance(domain, dict):
            domains[name] = ts.IndexDomain(json=domain)
        elif isinstance(domain, ts.DimExpression):
            domains[name] = ts.IndexDomain(shape=host_state[name].shape)[domain]
        elif not isinstance(domain, ts.IndexDomain):
            raise ValueError(f"Unknown domain: {domain} for {name!r}")

    start = time.time()
    rank_logger.info("Restoring checkpoint from %s", path)

    path = pathlib.Path(path) / tag
    metadata = ocp.StandardCheckpointer().metadata(path)
    with (path / "_METADATA").open() as f:
        use_zarr3 = json.load(f)["use_zarr3"]

    ts_context = ts.Context(
        {
            "file_io_concurrency": {"limit": 128},
            "cache_pool#ocdbt": {"total_bytes_limit": 100000000},
        }
    )

    unloaded_state = host_state.copy()

    plan: list[tuple[str, str, list[bool], int]] = []
    for checkpoint_name in tree_to_dict(metadata, keep_none=False).keys():
        name = checkpoint_name
        if rename is not None:
            name = rename(checkpoint_name)

        if host_state.get(name) is None:
            if not has_subtree(name, host_state):
                rank_logger.warning(
                    "Not loading %r from checkpoint because it's not in the initialized state", name
                )
            continue

        if not load_mask:
            mask = [True for _ in host_state[name].addressable_shards]
        else:
            mask = [shard.data.item() for shard in load_mask[name].addressable_shards]
        if any(mask):
            array = host_state[name]
            shard_nbytes = array.dtype.itemsize * math.prod(array.sharding.shard_shape(array.shape))
            plan.append((checkpoint_name, name, mask, shard_nbytes * sum(mask)))

        del unloaded_state[name]

    concurrent_bytes = int(concurrent_gb * 10**9) if concurrent_gb else None
    total_bytes = sum(nbytes for *_, nbytes in plan)
    max_leaf = max((nbytes for *_, nbytes in plan), default=0)

    batches: list[list[tuple[str, str, list[bool], int]]] = []
    if concurrent_bytes:
        cur: list[tuple[str, str, list[bool], int]] = []
        cur_bytes = 0
        for item in plan:
            nbytes = item[3]
            if cur and cur_bytes + nbytes > concurrent_bytes:
                batches.append(cur)
                cur, cur_bytes = [], 0
            cur.append(item)
            cur_bytes += nbytes
        if cur:
            batches.append(cur)
    elif plan:
        batches.append(plan)
    num_batches = len(batches)

    if concurrent_bytes:
        rank_logger.info(
            "load_checkpoint: restore read throttle ACTIVE concurrent_gb=%s "
            "(batch limit %.2fGiB): total=%.2fGiB in %d tensors, max_leaf=%.2fGiB, "
            "num_batches=%d (issue+drain one read batch at a time to bound host transient)",
            concurrent_gb,
            concurrent_bytes / (1 << 30),
            total_bytes / (1 << 30),
            len(plan),
            max_leaf / (1 << 30),
            num_batches,
        )
        if max_leaf > concurrent_bytes:
            rank_logger.warning(
                "load_checkpoint: largest tensor loads %.2fGiB > concurrent_gb %.2fGiB; "
                "it is issued as its own batch but still reads in one shot.",
                max_leaf / (1 << 30),
                concurrent_bytes / (1 << 30),
            )
    else:
        rank_logger.info(
            "load_checkpoint: no restore read throttle (concurrent_gb=None); issuing "
            "reads for all %d tensors (%.2fGiB) at once",
            len(plan),
            total_bytes / (1 << 30),
        )

    futures: dict[Any, tuple[str, str, int]] = {}

    def drain():
        for future in common._ready(list(futures), timeout=timeout):
            try:
                future.result()
            except Exception as e:
                logger.exception(e)
                checkpoint_name, name, _ = futures.pop(future, None)
                tensor = host_state[name]
                err = f"Checkpoint error from loading {path}. Error loading from {name} into {tensor.shape=}, {tensor.dtype=}."
                if checkpoint_name != name:
                    err = f"{err} (in checkpoint: {checkpoint_name})"
                logger.error(err)
                raise

            checkpoint_name, name, _ = futures.pop(future, None)
            if checkpoint_name != name:
                rank_logger.debug("Loaded %s (name in checkpoint: %s)", name, checkpoint_name)
            else:
                rank_logger.debug("Loaded %s", name)

    node_lock: _NodeBatchLock | None = None
    if _restore_node_serialize_enabled():
        node_lock = _NodeBatchLock(_node_lock_path())
        rank_logger.info(
            "restore node-serialize ACTIVE (flock per batch) lock=%s pid=%d num_batches=%d",
            node_lock._path,
            os.getpid(),
            num_batches,
        )

    try:
        for batch_index, batch in enumerate(batches, start=1):
            with node_lock if node_lock is not None else contextlib.nullcontext():
                stores = []
                for checkpoint_name, name, mask, _nbytes in batch:
                    info = ocp.type_handlers.ParamInfo(
                        name=checkpoint_name,
                        path=path / checkpoint_name,
                        parent_dir=path,
                        is_ocdbt_checkpoint=True,
                        use_zarr3=use_zarr3,
                    )
                    tspec = ocp.type_handlers.get_json_tspec_read(info, use_ocdbt=True)
                    t = ts.open(ts.Spec(tspec), open=True, context=ts_context).result()
                    if domains.get(name) is None and tuple(t.shape) != tuple(
                        host_state[name].shape
                    ):
                        raise ValueError(
                            f"Tensor {name!r}: checkpoint has shape {tuple(t.shape)}, "
                            f"but initialized state has shape {tuple(host_state[name].shape)}. "
                            f"Use 'no_loading' to skip this tensor or 'domains' to load a partial slice."
                        )
                    for s, future in enumerate(
                        _read_into_shards(t, host_state[name], mask, domains.get(name))
                    ):
                        futures[future] = (checkpoint_name, name, s)
                    stores.append(t)

                inflight_bytes = sum(nbytes for *_, nbytes in batch)
                rank_logger.info(
                    "load_checkpoint: draining read batch %d (%.2fGiB in %d futures)",
                    batch_index,
                    inflight_bytes / (1 << 30),
                    len(futures),
                )
                drain()
                del stores
            _release_batch_memory()
    finally:
        if node_lock is not None:
            node_lock.close()

    rank_logger.info("Loading checkpoint took %.2f sec", time.time() - start)


def broadcast_replicated(
    state: PyTree[jax.Array],
    axes: PyTree[jax.sharding.PartitionSpec],
    srcs: PyTree[jax.Array],
    mesh: jax.sharding.Mesh,
):
    shardings = jax.tree.map(lambda t: t.sharding, state)
    pspecs = jax.tree.map(lambda s: s.spec, shardings)
    donate_argnums = () if os.getenv("XAI_CHECKPOINT_BROADCAST_DONATE", "1") == "0" else (0,)

    @functools.partial(
        jax.jit, donate_argnums=donate_argnums, in_shardings=(shardings,), out_shardings=shardings
    )
    @functools.partial(shard_map, mesh=mesh, in_specs=(pspecs,), out_specs=pspecs, check_rep=False)
    def broadcast(tree):
        return jax.tree.map(common.pbroadcast, tree, axes, srcs)

    return jax.block_until_ready(broadcast(state))


def rename_tensor(name: str, rename_patterns: dict[str, str]):
    for pattern, repl in rename_patterns.items():
        result, count = re.subn(pattern, repl, name, flags=re.X)
        if count:
            return result
    return name
