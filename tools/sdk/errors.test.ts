import { describe, it, expect } from "vitest";
import {
  parseStreamError,
  isKnownStreamError,
  decodeContractError,
  describeContractError,
  MAX_STREAM_ERROR_CODE,
  streamErrorTable,
  isArchivedEntryError,
  isRetryableFailure,
  isLedgerSkewed,
  diagnoseFailure,
  retryKey,
} from "./errors";

describe("parseStreamError", () => {
  it("returns the correct message for each known error code", () => {
    expect(parseStreamError(1)).toBe(
      "StreamNotFound: no stream exists with the given id."
    );
    expect(parseStreamError(14)).toBe(
      "StreamTerminated: stream is cancelled or depleted."
    );
    expect(parseStreamError(25)).toBe(
      "TokenTransferFailed: the token contract rejected the transfer."
    );
    expect(parseStreamError(26)).toBe(
      "TokenMissing: the token address has no deployed contract."
    );
  });

  it("returns a fallback string for unknown error codes", () => {
    expect(parseStreamError(0)).toBe("Unknown stream error (code 0).");
    expect(parseStreamError(99)).toBe("Unknown stream error (code 99).");
    expect(parseStreamError(-1)).toBe("Unknown stream error (code -1).");
  });

  it("returns a fallback string for non-integer inputs", () => {
    expect(parseStreamError(NaN)).toBe("Unknown stream error (code NaN).");
    expect(parseStreamError(Infinity)).toBe(
      "Unknown stream error (code Infinity)."
    );
  });
});

describe("isKnownStreamError", () => {
  it("returns true for valid positive discriminants in the ABI table", () => {
    expect(isKnownStreamError(1)).toBe(true);
    expect(isKnownStreamError(31)).toBe(true);
  });

  it("returns false for zero, negative, and out-of-range codes", () => {
    expect(isKnownStreamError(0)).toBe(false);
    expect(isKnownStreamError(-1)).toBe(false);
    expect(isKnownStreamError(35)).toBe(false);
    expect(isKnownStreamError(NaN)).toBe(false);
  });
});

describe("decodeContractError / bounds", () => {
  it("extracts discriminants from diagnostics", () => {
    expect(decodeContractError("HostError: Error(Contract, #14)")).toBe(14);
    expect(decodeContractError("no error here")).toBeNull();
    expect(decodeContractError("x".repeat(600))).toBeNull();
    expect(describeContractError("Error(Contract, #99)")).toBe("Unknown stream error (code 99).");
  });

  it("table is dense over 1..=MAX and bounds are exact", () => {
    expect(Object.keys(streamErrorTable()).length).toBe(MAX_STREAM_ERROR_CODE);
    expect(isKnownStreamError(MAX_STREAM_ERROR_CODE)).toBe(true);
    expect(isKnownStreamError(MAX_STREAM_ERROR_CODE + 1)).toBe(false);
  });
});

describe("every error code 1..34", () => {
  it("maps each to a non-fallback message starting with its name", () => {
    for (let c = 1; c <= 34; c++) {
      expect(isKnownStreamError(c)).toBe(true);
      expect(parseStreamError(c)).toMatch(/^[A-Za-z]+: /);
    }
    expect(isKnownStreamError(35)).toBe(false);
    expect(isKnownStreamError(1.5)).toBe(false);
  });
});

describe("archival / retry / skew diagnostics", () => {
  it("detects archived entries", () => {
    expect(isArchivedEntryError("simulation failed: entry is archived")).toBe(true);
    expect(isArchivedEntryError("RestoreFootprint required")).toBe(true);
    expect(isArchivedEntryError("insufficient fee")).toBe(false);
  });

  it("detects retryable failures", () => {
    expect(isRetryableFailure("TRY_AGAIN_LATER")).toBe(true);
    expect(isRetryableFailure("request timed out")).toBe(true);
    expect(isRetryableFailure("Error(Contract, #7)")).toBe(false);
  });

  it("detects ledger skew at the boundary", () => {
    expect(isLedgerSkewed(100, 100)).toBe(false);
    expect(isLedgerSkewed(99, 100)).toBe(true);
    expect(isLedgerSkewed(99, 100, 1)).toBe(false);
    expect(() => isLedgerSkewed(-1, 1)).toThrow(RangeError);
    expect(() => isLedgerSkewed(1.5, 1)).toThrow(RangeError);
  });

  it("diagnoses failures deterministically", () => {
    expect(diagnoseFailure({ contractErrorCode: 14 }).action).toBe("escalate");
    expect(diagnoseFailure({ message: "entry archived" }).action).toBe("restore_footprint");
    expect(diagnoseFailure({ message: "TRY_AGAIN_LATER" }).action).toBe("retry");
    expect(diagnoseFailure({ message: "weird" }).action).toBe("escalate");
    expect(diagnoseFailure({}).action).toBe("none");
  });

  it("produces stable retry keys", () => {
    expect(retryKey("C1", "withdraw", [1n, 5])).toBe("C1|withdraw|1|5");
    expect(retryKey("C1", "withdraw", [1n, 5])).toBe(retryKey("C1", "withdraw", [1n, 5]));
  });
});
