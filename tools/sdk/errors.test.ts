import { parseStreamError, isKnownStreamError } from "./errors";

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
    expect(isKnownStreamError(32)).toBe(false);
    expect(isKnownStreamError(NaN)).toBe(false);
  });
});

import {
  decodeContractError,
  describeContractError,
  MAX_STREAM_ERROR_CODE,
  streamErrorTable,
} from "./errors";

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
