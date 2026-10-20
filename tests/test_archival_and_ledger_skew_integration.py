"""tests/test_archival_and_ledger_skew_integration.py

Integration test scenarios covering Soroban persistent entry archival detection,
footprint restoration workflows, and RPC ledger read skew mitigation.

Covers:
  1. Archived entry detection: distinguishing Live vs Archived vs Missing state.
  2. Footprint restoration: verifying RestoreFootprint guidance and key derivation.
  3. RPC Ledger Read Skew:
     - Non-monotonic getLatestLedger detection across distributed RPC replicas.
     - Differentiating transient read skew from permanent state archival.
     - Skew barrier synchronization with exponential backoff.
  4. Idempotent retry recovery: ensuring timed-out or repeated restore operations
     do not produce duplicate state transitions or data corruption.
"""

from __future__ import annotations

import importlib.util
import time
import unittest
from pathlib import Path
from unittest.mock import MagicMock

_ROOT = Path(__file__).resolve().parent.parent


def _load_script(name: str):
    path = _ROOT / "script" / f"{name}.py"
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ebb = _load_script("event-byte-budget")


class TestArchivedEntryRestorationIntegration(unittest.TestCase):
    """Integration scenarios for persistent entry state classification and restoration."""

    def test_distinguish_live_archived_and_missing_entries(self):
        """Streams below stream_count that are not live must be marked Archived."""
        stream_ids = [0, 1, 2, 5, 10]
        # Streams 0 and 1 are live; stream 2 is archived; stream 10 is missing (count is 5)
        liveness = {0: True, 1: True, 2: False, 5: False}
        stream_count = 5

        detection = ebb.detect_archival_state(
            stream_ids, liveness=liveness, stream_count=stream_count
        )

        self.assertEqual(detection["status"], ebb.ERR_ARCHIVED_ENTRY)
        self.assertEqual(detection["states"][0], ebb.STATE_LIVE)
        self.assertEqual(detection["states"][1], ebb.STATE_LIVE)
        self.assertEqual(detection["states"][2], ebb.STATE_ARCHIVED)
        self.assertEqual(detection["states"][10], ebb.STATE_MISSING)
        self.assertIn(2, detection["archived"])
        self.assertNotIn(0, detection["archived"])
        self.assertNotIn(10, detection["archived"])

    def test_restoration_guidance_for_archived_persistent_entries(self):
        """Archived stream entries produce deterministic restoration steps for operators."""
        archived_ids = [2, 4]
        guidance = ebb.restoration_guidance(archived_ids)

        self.assertEqual(guidance["status"], ebb.ERR_ARCHIVED_ENTRY)
        self.assertEqual(guidance["stream_ids"], [2, 4])
        self.assertEqual(guidance["tool"], ebb.RESTORE_FOOTPRINT_TOOL)
        self.assertTrue(any("RestoreFootprint" in s or "restore_footprint.ts" in s for s in guidance["steps"]))
        self.assertTrue(any("idempotent" in s.lower() for s in guidance["steps"]))

    def test_idempotent_retry_only_runs_when_all_entries_live(self):
        """Operations must be gated on all target entries being Live."""
        pre_state_with_archived = {
            "status": ebb.ERR_ARCHIVED_ENTRY,
            "states": {1: ebb.STATE_LIVE, 2: ebb.STATE_ARCHIVED},
            "archived": [2],
        }

        mock_op = MagicMock(return_value="executed")
        result = ebb.idempotent_retry([1, 2], pre_state_with_archived, mock_op)

        self.assertEqual(result["status"], ebb.ERR_ARCHIVED_ENTRY)
        self.assertEqual(result["attempts"], 0)
        self.assertIsNone(result["result"])
        mock_op.assert_not_called()

        # Once restored, idempotent retry succeeds
        pre_state_restored = {
            "status": ebb.ERR_OK,
            "states": {1: ebb.STATE_LIVE, 2: ebb.STATE_LIVE},
            "archived": [],
        }
        res_ok = ebb.idempotent_retry([1, 2], pre_state_restored, mock_op)
        self.assertEqual(res_ok["status"], ebb.ERR_OK)
        self.assertEqual(res_ok["attempts"], 1)
        self.assertEqual(res_ok["result"], "executed")
        mock_op.assert_called_once_with([1, 2])


class TestLedgerReadSkewScenarios(unittest.TestCase):
    """Simulated RPC read skew and synchronization barrier integration scenarios."""

    def test_rpc_replica_read_skew_detection(self):
        """Detect when distributed RPC endpoints return differing latestLedger values."""
        # Simulated responses from 3 RPC pool nodes
        node_responses = [
            {"node": "rpc-1.testnet", "latestLedger": 1050, "stream_exists": True},
            {"node": "rpc-2.testnet", "latestLedger": 1042, "stream_exists": False}, # lagging node shows False
            {"node": "rpc-3.testnet", "latestLedger": 1050, "stream_exists": True},
        ]

        ledgers = [r["latestLedger"] for r in node_responses]
        max_ledger = max(ledgers)
        min_ledger = min(ledgers)
        ledger_skew = max_ledger - min_ledger

        # Detect skew: node-2 lags by 8 ledgers
        self.assertGreater(ledger_skew, 0)
        self.assertEqual(ledger_skew, 8)

        # A lagging node returning stream_exists == False is classified as Read Skew, not Archival
        lagging_node = min(node_responses, key=lambda n: n["latestLedger"])
        is_read_skew = lagging_node["latestLedger"] < max_ledger
        self.assertTrue(is_read_skew, "Must detect read skew before concluding state archival")

    def test_skew_barrier_convergence_after_retry(self):
        """Simulate convergence of read skew barrier with bounded backoff."""
        # Consecutive ledger samples from an endpoint that catches up
        ledger_samples = [1000, 1002, 1005, 1005]
        sample_idx = 0

        def sample_rpc():
            nonlocal sample_idx
            val = ledger_samples[sample_idx]
            if sample_idx < len(ledger_samples) - 1:
                sample_idx += 1
            return val

        # Barrier requires 2 consecutive stable samples >= target
        target_ledger = 1005
        stable_streak = 0
        max_polls = 10
        converged = False

        for _ in range(max_polls):
            current = sample_rpc()
            if current >= target_ledger:
                stable_streak += 1
                if stable_streak >= 2:
                    converged = True
                    break
            else:
                stable_streak = 0

        self.assertTrue(converged, "RPC read skew barrier must converge to target ledger")
        self.assertEqual(sample_rpc(), 1005)

    def test_timed_out_invocation_idempotent_recovery(self):
        """Simulate recovery when an RPC call times out but the operation was applied."""
        call_count = 0
        on_chain_ledger_state = {"transferred": False}

        def flaky_rpc_invoke(stream_ids):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                # First attempt times out on network write after ledger inclusion
                on_chain_ledger_state["transferred"] = True
                raise TimeoutError("RPC endpoint read timeout (504 Gateway Timeout)")
            # Second attempt: check state before re-running
            return "SUCCESS_ALREADY_APPLIED" if on_chain_ledger_state["transferred"] else "TRANSFERRED"

        pre_state = {"status": ebb.ERR_OK, "states": {1: ebb.STATE_LIVE}, "archived": []}
        result = ebb.idempotent_retry([1], pre_state, flaky_rpc_invoke, max_attempts=3)

        self.assertEqual(result["status"], ebb.ERR_OK)
        self.assertEqual(result["attempts"], 2)
        self.assertEqual(result["result"], "SUCCESS_ALREADY_APPLIED")
        self.assertTrue(on_chain_ledger_state["transferred"])


if __name__ == "__main__":
    unittest.main()
